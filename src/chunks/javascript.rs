use super::{Candidate, ChunkExtractor, Context, assets};
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};

pub struct Imports;
pub struct References;
pub struct Vite;

pub(super) fn static_string(expression: &Expression<'_>) -> Option<String> {
    match expression.get_inner_expression() {
        Expression::StringLiteral(literal) => Some(literal.value.to_string()),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .iter()
            .map(|quasi| quasi.value.cooked.as_ref().map(ToString::to_string))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.concat()),
        _ => None,
    }
}

impl ChunkExtractor for Imports {
    fn name(&self) -> &'static str {
        "imports"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        if let Some(program) = context.program {
            ImportVisitor(candidates).visit_program(program);
        }
    }
}

struct ImportVisitor<'s>(&'s mut Vec<Candidate>);
impl<'a> Visit<'a> for ImportVisitor<'_> {
    fn visit_import_expression(&mut self, import: &ImportExpression<'a>) {
        if let Some(path) = static_string(&import.source) {
            self.0.push(Candidate::new(path, Some(import.span)));
        }
        walk::walk_import_expression(self, import);
    }
    fn visit_import_declaration(&mut self, import: &ImportDeclaration<'a>) {
        self.0.push(Candidate::new(
            import.source.value.to_string(),
            Some(import.source.span),
        ));
        walk::walk_import_declaration(self, import);
    }
    fn visit_export_named_declaration(&mut self, export: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &export.source {
            self.0
                .push(Candidate::new(source.value.to_string(), Some(source.span)));
        }
        walk::walk_export_named_declaration(self, export);
    }
    fn visit_export_all_declaration(&mut self, export: &ExportAllDeclaration<'a>) {
        self.0.push(Candidate::new(
            export.source.value.to_string(),
            Some(export.source.span),
        ));
        walk::walk_export_all_declaration(self, export);
    }
}

impl ChunkExtractor for References {
    fn name(&self) -> &'static str {
        "references"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        if let Some(program) = context.program {
            ReferenceVisitor(candidates).visit_program(program);
        }
    }
}
struct ReferenceVisitor<'s>(&'s mut Vec<Candidate>);
impl<'a> Visit<'a> for ReferenceVisitor<'_> {
    fn visit_string_literal(&mut self, literal: &StringLiteral<'a>) {
        if assets::is_direct_reference(literal.value.trim()) {
            self.0.push(Candidate::new(
                literal.value.to_string(),
                Some(literal.span),
            ));
        }
    }
    fn visit_template_literal(&mut self, template: &TemplateLiteral<'a>) {
        if template.expressions.is_empty()
            && let Some(value) = template
                .quasis
                .first()
                .and_then(|quasi| quasi.value.cooked.as_ref())
            && assets::is_direct_reference(value.trim())
        {
            self.0
                .push(Candidate::new(value.to_string(), Some(template.span)));
        }
        walk::walk_template_literal(self, template);
    }
}

impl ChunkExtractor for Vite {
    fn name(&self) -> &'static str {
        "vite"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        if let Some(program) = context.program {
            ViteVisitor(candidates).visit_program(program);
        }
    }
}
struct ViteVisitor<'s>(&'s mut Vec<Candidate>);
impl<'a> Visit<'a> for ViteVisitor<'_> {
    fn visit_variable_declarator(&mut self, declaration: &VariableDeclarator<'a>) {
        if declaration
            .id
            .get_binding_identifier()
            .is_some_and(|id| id.name == "__vite__mapDeps")
            && let Some(init) = &declaration.init
        {
            ArrayVisitor(self.0).visit_expression(init);
        }
        walk::walk_variable_declarator(self, declaration);
    }
    fn visit_assignment_expression(&mut self, assignment: &AssignmentExpression<'a>) {
        if matches!(&assignment.left, AssignmentTarget::AssignmentTargetIdentifier(id) if id.name == "__vite__mapDeps")
        {
            ArrayVisitor(self.0).visit_expression(&assignment.right);
        }
        walk::walk_assignment_expression(self, assignment);
    }
    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if matches!(&call.callee, Expression::Identifier(id) if id.name == "__vitePreload")
            && let Some(expression) = call
                .arguments
                .get(1)
                .and_then(|argument| argument.as_expression())
        {
            ArrayVisitor(self.0).visit_expression(expression);
        }
        walk::walk_call_expression(self, call);
    }
}
struct ArrayVisitor<'s>(&'s mut Vec<Candidate>);
impl<'a> Visit<'a> for ArrayVisitor<'_> {
    fn visit_array_expression(&mut self, array: &ArrayExpression<'a>) {
        for element in &array.elements {
            if let Some(expression) = element.as_expression()
                && let Some(value) = static_string(expression)
            {
                self.0.push(Candidate::new(value, Some(array.span)));
            }
        }
        walk::walk_array_expression(self, array);
    }
}
