use super::QueryIndex;
use crate::source::Error;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

/// Compiled CSS-style selectors over ESTree node types and arbitrary field paths.
pub struct Selector {
    groups: Vec<Vec<Step>>,
}
struct Step {
    compound: Compound,
    child: bool,
}
struct Compound {
    kind: Option<String>,
    field: Option<String>,
    predicates: Vec<Predicate>,
}
enum Predicate {
    Attribute(Attribute),
    Not(Selector),
    Is(Selector),
    Has { selector: Selector, direct: bool },
}
struct Attribute {
    path: String,
    operator: Operator,
    value: Expected,
}
#[derive(Clone, Copy)]
enum Operator {
    Exists,
    Equal,
    NotEqual,
    Contains,
    Prefix,
    Suffix,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
}
enum Expected {
    Value(Value),
    Regex(Regex),
}

impl Selector {
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.len() > 16_384 {
            return Err(Error("selector exceeds 16384 bytes".into()));
        }
        let mut parser = Syntax {
            text,
            offset: 0,
            depth: 0,
            steps: 0,
        };
        let selector = parser.list()?;
        parser.space();
        if parser.offset != text.len() {
            return parser.error("unexpected selector token");
        }
        Ok(selector)
    }
}

struct Syntax<'a> {
    text: &'a str,
    offset: usize,
    depth: usize,
    steps: usize,
}
impl Syntax<'_> {
    fn rest(&self) -> &str {
        &self.text[self.offset..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn eat(&mut self, text: &str) -> bool {
        if self.rest().starts_with(text) {
            self.offset += text.len();
            true
        } else {
            false
        }
    }
    fn space(&mut self) -> bool {
        let start = self.offset;
        while self.peek().is_some_and(char::is_whitespace) {
            self.offset += self.peek().unwrap().len_utf8();
        }
        self.offset > start
    }
    fn error<T>(&self, message: &str) -> Result<T, Error> {
        Err(Error(format!(
            "selector error at byte {}: {message}",
            self.offset
        )))
    }
    fn word(&mut self, path: bool) -> String {
        let start = self.offset;
        while self.peek().is_some_and(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '$')
                || (path && matches!(c, '.' | '*' | '@'))
        }) {
            // A '*' after a field name starts the contains operator. A '*' at
            // the beginning of a path component is a wildcard (e.g. args.*=x).
            if path
                && self.rest().starts_with("*=")
                && self.offset > start
                && !self.text[..self.offset].ends_with('.')
            {
                break;
            }
            self.offset += 1;
        }
        self.text[start..self.offset].into()
    }
    fn list(&mut self) -> Result<Selector, Error> {
        let mut groups = Vec::new();
        loop {
            self.space();
            let mut steps = vec![Step {
                compound: self.compound()?,
                child: false,
            }];
            loop {
                let spaced = self.space();
                if matches!(self.peek(), None | Some(',' | ')')) {
                    break;
                }
                let child = self.eat(">");
                if !child && !spaced {
                    return self.error("expected whitespace, '>', or ',' between selectors");
                }
                self.space();
                steps.push(Step {
                    compound: self.compound()?,
                    child,
                });
            }
            groups.push(steps);
            if !self.eat(",") {
                break;
            }
        }
        Ok(Selector { groups })
    }
    fn compound(&mut self) -> Result<Compound, Error> {
        self.steps += 1;
        if self.steps > 64 {
            return self.error("selector exceeds 64 compound selectors");
        }
        let wildcard = self.eat("*");
        let kind = if wildcard {
            None
        } else {
            let word = self.word(false);
            (!word.is_empty()).then_some(word)
        };
        let mut result = Compound {
            kind,
            field: None,
            predicates: Vec::new(),
        };
        let start = self.offset;
        loop {
            if self.eat(".") {
                let field = self.word(true);
                if !valid_path(&field) || field.contains('@') {
                    return self.error("expected a dotted edge field after '.'");
                }
                if result.field.is_some() {
                    return self.error("only one edge field is allowed per compound selector");
                }
                result.field = Some(field);
            } else if self.eat("[") {
                result
                    .predicates
                    .push(Predicate::Attribute(self.attribute()?));
            } else if self.eat(":") {
                let name = self.word(false);
                if !matches!(name.as_str(), "has" | "not" | "is") {
                    return self.error("supported pseudo-selectors are :has(), :not(), and :is()");
                }
                if !self.eat("(") {
                    return self.error("expected '(' after pseudo-selector");
                }
                self.depth += 1;
                if self.depth > 16 {
                    return self.error("pseudo-selector nesting exceeds 16");
                }
                self.space();
                let direct = name == "has" && self.eat(">");
                let selector = self.list()?;
                if !self.eat(")") {
                    return self.error("expected ')' after pseudo-selector");
                }
                self.depth -= 1;
                result.predicates.push(match name.as_str() {
                    "not" => Predicate::Not(selector), "is" => Predicate::Is(selector),
                    _ => {
                        if selector.groups.iter().any(|steps| steps.len() != 1) { return self.error(":has() accepts compound selectors; put tree relations outside :has()"); }
                        Predicate::Has { selector, direct }
                    }
                });
            } else {
                break;
            }
        }
        if start == self.offset && result.kind.is_none() && !wildcard {
            return self.error("expected a node type, '*', attribute, field, or pseudo-selector");
        }
        Ok(result)
    }
    fn attribute(&mut self) -> Result<Attribute, Error> {
        self.space();
        let path = self.word(true);
        if !valid_path(&path) {
            return self.error("expected a dotted AST field path");
        }
        if let Some(meta) = path.strip_prefix('@')
            && !matches!(meta, "id" | "field" | "text" | "start" | "end" | "line")
        {
            return self
                .error("unknown metadata field (use @id, @field, @text, @start, @end, or @line)");
        }
        self.space();
        if self.eat("]") {
            return Ok(Attribute {
                path,
                operator: Operator::Exists,
                value: Expected::Value(Value::Null),
            });
        }
        let operator = [
            ("!=", Operator::NotEqual),
            ("*=", Operator::Contains),
            ("^=", Operator::Prefix),
            ("$=", Operator::Suffix),
            (">=", Operator::GreaterEqual),
            ("<=", Operator::LessEqual),
            ("=", Operator::Equal),
            (">", Operator::Greater),
            ("<", Operator::Less),
        ]
        .into_iter()
        .find_map(|(token, operator)| self.eat(token).then_some(operator));
        let Some(operator) = operator else {
            return self.error("unknown attribute operator");
        };
        self.space();
        let value = match self.peek() {
            Some('"' | '\'') => Expected::Value(Value::String(self.string()?)),
            Some('/') => {
                if !matches!(operator, Operator::Equal | Operator::NotEqual) {
                    return self.error("regex attributes require '=' or '!='");
                }
                self.offset += 1;
                let start = self.offset;
                let mut escaped = false;
                let mut class = false;
                while let Some(c) = self.peek() {
                    if c == '/' && !escaped && !class {
                        break;
                    }
                    if !escaped && c == '[' {
                        class = true;
                    }
                    if !escaped && c == ']' {
                        class = false;
                    }
                    escaped = c == '\\' && !escaped;
                    self.offset += c.len_utf8();
                }
                let pattern = self.text[start..self.offset].replace("\\/", "/");
                if !self.eat("/") {
                    return self.error("unterminated regex attribute");
                }
                let flags = self.word(false);
                if flags.chars().any(|flag| !matches!(flag, 'i' | 'm' | 's')) {
                    return self.error("regex flags may contain only i, m, and s");
                }
                let regex = RegexBuilder::new(&pattern)
                    .case_insensitive(flags.contains('i'))
                    .multi_line(flags.contains('m'))
                    .dot_matches_new_line(flags.contains('s'))
                    .build()
                    .map_err(|error| Error(format!("invalid selector regex: {error}")))?;
                Expected::Regex(regex)
            }
            _ => {
                let start = self.offset;
                while self.peek().is_some_and(|c| c != ']' && !c.is_whitespace()) {
                    self.offset += self.peek().unwrap().len_utf8();
                }
                let value = &self.text[start..self.offset];
                if value.is_empty() {
                    return self.error("expected an attribute value");
                }
                Expected::Value(
                    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into())),
                )
            }
        };
        self.space();
        if !self.eat("]") {
            return self.error("expected ']' after attribute");
        }
        Ok(Attribute {
            path,
            operator,
            value,
        })
    }
    fn string(&mut self) -> Result<String, Error> {
        let quote = self.peek().unwrap();
        self.offset += 1;
        let mut json = String::from("\"");
        let mut closed = false;
        while let Some(c) = self.peek() {
            self.offset += c.len_utf8();
            if c == quote {
                closed = true;
                break;
            }
            if c == '\\' {
                let Some(next) = self.peek() else {
                    break;
                };
                self.offset += next.len_utf8();
                if next == '\'' {
                    json.push('\'');
                } else {
                    json.push('\\');
                    json.push(next);
                }
            } else if c == '"' {
                json.push_str("\\\"");
            } else {
                json.push(c);
            }
        }
        if !closed {
            return self.error("unterminated quoted attribute");
        }
        json.push('"');
        serde_json::from_str(&json)
            .map_err(|error| Error(format!("invalid quoted attribute: {error}")))
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('.')
            .all(|part| !part.is_empty() && (!part.contains('*') || part == "*"))
}

struct Budget {
    nodes: usize,
    text_bytes: usize,
}
pub(super) fn evaluate(selector: &Selector, index: &QueryIndex<'_>) -> Result<Vec<bool>, Error> {
    eval(
        selector,
        index,
        &mut Budget {
            nodes: 10_000_000,
            text_bytes: 256_000_000,
        },
    )
}
fn eval(
    selector: &Selector,
    index: &QueryIndex<'_>,
    budget: &mut Budget,
) -> Result<Vec<bool>, Error> {
    let mut matches = vec![false; index.nodes.len()];
    for steps in &selector.groups {
        let mut previous = vec![false; index.nodes.len()];
        for (position, step) in steps.iter().enumerate() {
            let mut current = compound(&step.compound, index, budget)?;
            if position > 0 {
                let mut ancestors = vec![false; index.nodes.len()];
                for id in &index.order {
                    let parent = index.nodes[*id].parent;
                    ancestors[*id] = parent.is_some_and(|parent| {
                        previous[parent] || (!step.child && ancestors[parent])
                    });
                    current[*id] &= ancestors[*id];
                }
            }
            previous = current;
        }
        for (found, group) in matches.iter_mut().zip(previous) {
            *found |= group;
        }
    }
    Ok(matches)
}
fn compound(
    compound: &Compound,
    index: &QueryIndex<'_>,
    budget: &mut Budget,
) -> Result<Vec<bool>, Error> {
    budget.nodes = budget.nodes.checked_sub(index.nodes.len()).ok_or_else(|| {
        Error("query evaluation budget exceeded; narrow or split the selector".into())
    })?;
    let mut matches = vec![true; index.nodes.len()];
    for (id, node) in index.nodes.iter().enumerate() {
        matches[id] = compound.kind.as_ref().is_none_or(|kind| node.kind == *kind)
            && compound.field.as_ref().is_none_or(|field| {
                node.field
                    .as_deref()
                    .is_some_and(|actual| edge_matches(actual, field))
            });
        if !matches[id] {
            continue;
        }
        for predicate in &compound.predicates {
            let Predicate::Attribute(attribute) = predicate else {
                continue;
            };
            if attribute.path == "@text" {
                budget.text_bytes = budget
                    .text_bytes
                    .checked_sub(node.span.size() as usize)
                    .ok_or_else(|| {
                        Error("source-text query budget exceeded; add a node type filter".into())
                    })?;
            }
            let values = index.lookup(id, &attribute.path);
            if !values.iter().any(|value| {
                if matches!(attribute.operator, Operator::Exists) {
                    return true;
                }
                let Some(value) = value.scalar() else {
                    return false;
                };
                compare(value, attribute)
            }) {
                matches[id] = false;
                break;
            }
        }
    }
    for predicate in &compound.predicates {
        let (filter, negate) = match predicate {
            Predicate::Attribute(_) => continue,
            Predicate::Not(selector) => (eval(selector, index, budget)?, true),
            Predicate::Is(selector) => (eval(selector, index, budget)?, false),
            Predicate::Has { selector, direct } => {
                let inner = eval(selector, index, budget)?;
                let mut has = vec![false; index.nodes.len()];
                // Arena order is post-order: every child precedes its parent.
                for id in 0..index.nodes.len() {
                    if (inner[id] || (!direct && has[id]))
                        && let Some(parent) = index.nodes[id].parent
                    {
                        has[parent] = true;
                    }
                }
                (has, false)
            }
        };
        for (found, filter) in matches.iter_mut().zip(filter) {
            *found &= filter != negate;
        }
    }
    Ok(matches)
}

fn edge_matches(actual: &str, expected: &str) -> bool {
    let mut actual = actual.split('.');
    expected.split('.').all(|part| {
        actual
            .next()
            .is_some_and(|actual| part == "*" || part == actual)
    })
}
fn compare(value: &Value, attribute: &Attribute) -> bool {
    let Expected::Value(expected) = &attribute.value else {
        let Expected::Regex(regex) = &attribute.value else {
            unreachable!()
        };
        let found = value.as_str().is_some_and(|value| regex.is_match(value));
        return value.is_string()
            && if matches!(attribute.operator, Operator::NotEqual) {
                !found
            } else {
                found
            };
    };
    match attribute.operator {
        Operator::Equal => equal(value, expected),
        Operator::NotEqual => !equal(value, expected),
        Operator::Contains | Operator::Prefix | Operator::Suffix => value
            .as_str()
            .zip(expected.as_str())
            .is_some_and(|(value, expected)| match attribute.operator {
                Operator::Contains => value.contains(expected),
                Operator::Prefix => value.starts_with(expected),
                _ => value.ends_with(expected),
            }),
        Operator::Greater | Operator::Less | Operator::GreaterEqual | Operator::LessEqual => value
            .as_f64()
            .zip(expected.as_f64())
            .is_some_and(|(value, expected)| match attribute.operator {
                Operator::Greater => value > expected,
                Operator::Less => value < expected,
                Operator::GreaterEqual => value >= expected,
                _ => value <= expected,
            }),
        Operator::Exists => true,
    }
}

fn equal(value: &Value, expected: &Value) -> bool {
    if value.is_number() && expected.is_number() {
        value.as_f64() == expected.as_f64()
    } else {
        value == expected
    }
}
