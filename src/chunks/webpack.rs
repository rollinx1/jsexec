use super::{Candidate, ChunkExtractor, Context, javascript::static_string};
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator};
use std::collections::{BTreeMap, BTreeSet};

pub struct Webpack;
impl ChunkExtractor for Webpack {
    fn name(&self) -> &'static str {
        "webpack"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        let Some(program) = context.program else {
            return;
        };
        let mut runtime = RuntimeData::default();
        runtime.visit_program(program);
        RuntimeVisitor {
            data: &runtime,
            candidates,
        }
        .visit_program(program);
    }
}

#[derive(Default)]
struct RuntimeData {
    paths: BTreeMap<String, BTreeSet<Option<String>>>,
    ids: BTreeMap<String, BTreeSet<ChunkId>>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ChunkId {
    text: String,
    numeric: bool,
}

// Associate public paths and explicit lazy-load IDs with their runtime object.
impl<'a> Visit<'a> for RuntimeData {
    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        if let Some((runtime, property)) = assignment_member(&assignment.left)
            && property == "p"
        {
            let path = (assignment.operator == AssignmentOperator::Assign)
                .then(|| static_string(&assignment.right))
                .flatten();
            self.paths.entry(runtime).or_default().insert(path);
        }
        walk::walk_assignment_expression(self, assignment);
    }
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Some((runtime, property)) = expression_member(&call.callee)
            && property == "e"
            && let Some(argument) = call
                .arguments
                .first()
                .and_then(|argument| argument.as_expression())
            && let Some(id) = literal_key(argument)
        {
            self.ids.entry(runtime).or_default().insert(id);
        }
        walk::walk_call_expression(self, call);
    }
}

struct RuntimeVisitor<'s> {
    data: &'s RuntimeData,
    candidates: &'s mut Vec<Candidate>,
}
impl<'a> Visit<'a> for RuntimeVisitor<'_> {
    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        if assignment.operator == AssignmentOperator::Assign
            && let Some((runtime, property)) = assignment_member(&assignment.left)
            && property == "u"
            && let Some((parameter, expression)) = filename_expression(&assignment.right)
        {
            let mut ids = IdCollector {
                parameter,
                ids: self.data.ids.get(&runtime).cloned().unwrap_or_default(),
            };
            ids.visit_expression(expression);
            let path = self
                .data
                .paths
                .get(&runtime)
                .filter(|paths| paths.len() == 1)
                .and_then(|paths| paths.first())
                .and_then(|path| path.as_ref());
            // A constant filename is valid without a chunk-ID map.
            if ids.ids.is_empty()
                && let Some(value) = static_string(expression)
            {
                self.add(value, path, assignment.span);
            }
            for id in ids.ids {
                if let Some(Value::String(value)) = evaluate(expression, parameter, &id, 0) {
                    self.add(value, path, assignment.span);
                }
            }
        }
        walk::walk_assignment_expression(self, assignment);
    }
}
impl RuntimeVisitor<'_> {
    fn add(&mut self, filename: String, public_path: Option<&String>, span: oxc_span::Span) {
        // Webpack concatenates p + u(id). URL joining here would change runtime meaning.
        let reference = match public_path {
            Some(path) => format!("{path}{filename}"),
            None => filename,
        };
        self.candidates.push(Candidate::new(reference, Some(span)));
    }
}

fn assignment_member(target: &AssignmentTarget<'_>) -> Option<(String, String)> {
    match target {
        AssignmentTarget::StaticMemberExpression(member) => Some((
            member.object.get_identifier_reference()?.name.to_string(),
            member.property.name.to_string(),
        )),
        AssignmentTarget::ComputedMemberExpression(member) => Some((
            member.object.get_identifier_reference()?.name.to_string(),
            static_string(&member.expression)?,
        )),
        _ => None,
    }
}
fn expression_member(expression: &Expression<'_>) -> Option<(String, String)> {
    match expression.get_inner_expression() {
        Expression::StaticMemberExpression(member) => Some((
            member.object.get_identifier_reference()?.name.to_string(),
            member.property.name.to_string(),
        )),
        Expression::ComputedMemberExpression(member) => Some((
            member.object.get_identifier_reference()?.name.to_string(),
            static_string(&member.expression)?,
        )),
        _ => None,
    }
}

fn filename_expression<'s, 'a>(
    expression: &'s Expression<'a>,
) -> Option<(&'s str, &'s Expression<'a>)> {
    let (params, body, expression_body) = match expression.get_inner_expression() {
        Expression::ArrowFunctionExpression(arrow) => {
            (&arrow.params, &arrow.body, arrow.expression)
        }
        Expression::FunctionExpression(function) => {
            (&function.params, function.body.as_ref()?, false)
        }
        _ => return None,
    };
    let parameter = match params.items.first() {
        Some(parameter) => parameter.pattern.get_binding_identifier()?.name.as_str(),
        None => "",
    };
    // Multiple statements could alter the parameter or introduce branching; don't guess.
    if body.statements.len() != 1 {
        return None;
    }
    let value = match &body.statements[0] {
        Statement::ExpressionStatement(statement) if expression_body => &statement.expression,
        Statement::ReturnStatement(statement) => statement.argument.as_ref()?,
        _ => return None,
    };
    Some((parameter, value))
}

struct IdCollector<'s> {
    parameter: &'s str,
    ids: BTreeSet<ChunkId>,
}
impl<'a> Visit<'a> for IdCollector<'_> {
    fn visit_computed_member_expression(&mut self, member: &ComputedMemberExpression<'a>) {
        if member
            .expression
            .get_identifier_reference()
            .is_some_and(|id| id.name == self.parameter)
            && let Expression::ObjectExpression(object) = member.object.get_inner_expression()
        {
            for property in &object.properties {
                if let ObjectPropertyKind::ObjectProperty(property) = property
                    && !property.computed
                    && let Some(key) = property.key.static_name()
                {
                    // Explicit .e() calls retain the argument's actual type. For map-only
                    // IDs, canonical numeric keys follow Webpack's numeric-ID convention.
                    if !self.ids.iter().any(|id| id.text == key) {
                        let text = key.to_string();
                        let numeric = text
                            .parse::<f64>()
                            .is_ok_and(|number| number.to_string() == text);
                        self.ids.insert(ChunkId { text, numeric });
                    }
                }
            }
        }
        walk::walk_computed_member_expression(self, member);
    }
    fn visit_binary_expression(&mut self, binary: &BinaryExpression<'a>) {
        if binary.operator.is_equality() {
            for (identifier, literal) in
                [(&binary.left, &binary.right), (&binary.right, &binary.left)]
            {
                if identifier
                    .get_identifier_reference()
                    .is_some_and(|id| id.name == self.parameter)
                    && let Some(id) = literal_key(literal)
                    && !self.ids.iter().any(|known| known.text == id.text)
                {
                    self.ids.insert(id);
                }
            }
        }
        walk::walk_binary_expression(self, binary);
    }
}

fn literal_key(expression: &Expression<'_>) -> Option<ChunkId> {
    match expression.get_inner_expression() {
        Expression::StringLiteral(literal) => Some(ChunkId {
            text: literal.value.to_string(),
            numeric: false,
        }),
        Expression::NumericLiteral(literal) => Some(ChunkId {
            text: literal.value.to_string(),
            numeric: true,
        }),
        _ => None,
    }
}

#[derive(PartialEq)]
enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Undefined,
}
impl Value {
    fn text(&self) -> Option<String> {
        match self {
            Self::String(value) => Some(value.clone()),
            Self::Number(value) => Some(value.to_string()),
            Self::Bool(value) => Some(value.to_string()),
            // Incomplete map entries must not produce fabricated filenames containing "undefined".
            Self::Undefined => None,
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Self::String(value) => !value.is_empty(),
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Bool(value) => *value,
            Self::Undefined => false,
        }
    }
}

// A bounded evaluator of a small, side-effect-free AST subset. Never executes JavaScript.
fn evaluate(
    expression: &Expression<'_>,
    parameter: &str,
    id: &ChunkId,
    depth: usize,
) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    let recurse = |expression| evaluate(expression, parameter, id, depth + 1);
    match expression.get_inner_expression() {
        Expression::StringLiteral(literal) => Some(Value::String(literal.value.to_string())),
        Expression::NumericLiteral(literal) => Some(Value::Number(literal.value)),
        Expression::BooleanLiteral(literal) => Some(Value::Bool(literal.value)),
        Expression::Identifier(identifier) if identifier.name == parameter => {
            if id.numeric {
                Some(Value::Number(id.text.parse().ok()?))
            } else {
                Some(Value::String(id.text.clone()))
            }
        }
        Expression::TemplateLiteral(template) => {
            let mut value = String::new();
            for (index, quasi) in template.quasis.iter().enumerate() {
                value.push_str(quasi.value.cooked.as_ref()?.as_str());
                if let Some(expression) = template.expressions.get(index) {
                    value.push_str(&recurse(expression)?.text()?);
                }
            }
            Some(Value::String(value))
        }
        Expression::ComputedMemberExpression(member) => {
            let Expression::ObjectExpression(object) = member.object.get_inner_expression() else {
                return None;
            };
            let key = recurse(&member.expression)?.text()?;
            let mut result = Value::Undefined;
            for property in &object.properties {
                // Spreads, getters and computed keys can change lookup semantics.
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    return None;
                };
                if property.computed || property.method || property.kind != PropertyKind::Init {
                    return None;
                }
                if property.key.static_name()?.as_ref() == key {
                    result = recurse(&property.value)?;
                }
            }
            Some(result)
        }
        Expression::BinaryExpression(binary) => {
            let left = recurse(&binary.left)?;
            let right = recurse(&binary.right)?;
            match binary.operator {
                BinaryOperator::Addition => match (&left, &right) {
                    (Value::Number(left), Value::Number(right)) => {
                        Some(Value::Number(left + right))
                    }
                    (Value::String(_), _) | (_, Value::String(_)) => {
                        Some(Value::String(left.text()? + &right.text()?))
                    }
                    _ => None,
                },
                BinaryOperator::StrictEquality => Some(Value::Bool(left == right)),
                BinaryOperator::StrictInequality => Some(Value::Bool(left != right)),
                BinaryOperator::Equality | BinaryOperator::Inequality => {
                    let equal = match (&left, &right) {
                        (Value::String(string), Value::Number(number))
                        | (Value::Number(number), Value::String(string)) => {
                            string.parse::<f64>().ok() == Some(*number)
                        }
                        _ => left == right,
                    };
                    Some(Value::Bool(
                        if binary.operator == BinaryOperator::Equality {
                            equal
                        } else {
                            !equal
                        },
                    ))
                }
                _ => None,
            }
        }
        Expression::LogicalExpression(logical) => {
            let left = recurse(&logical.left)?;
            match logical.operator {
                LogicalOperator::Or if !left.truthy() => recurse(&logical.right),
                LogicalOperator::And if left.truthy() => recurse(&logical.right),
                LogicalOperator::Coalesce if left == Value::Undefined => recurse(&logical.right),
                _ => Some(left),
            }
        }
        Expression::ConditionalExpression(conditional) => {
            if recurse(&conditional.test)?.truthy() {
                recurse(&conditional.consequent)
            } else {
                recurse(&conditional.alternate)
            }
        }
        _ => None,
    }
}
