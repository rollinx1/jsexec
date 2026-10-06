mod index;
mod selector;

use crate::source::{Diagnostic, Error, Location, LocationIndex, Source};
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
pub use selector::Selector;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// Repeated schema names, kinds, and scalar strings share storage within an index.
// Fields use sorted flat slices instead of a separately allocated tree per node.
pub(super) struct Fields(Box<[(std::sync::Arc<str>, Field)]>);
impl Fields {
    fn get(&self, key: &str) -> Option<&Field> {
        self.0
            .binary_search_by(|(name, _)| name.as_ref().cmp(key))
            .ok()
            .map(|i| &self.0[i].1)
    }
    fn iter(&self) -> impl Iterator<Item = &(std::sync::Arc<str>, Field)> {
        self.0.iter()
    }
    fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(key, _)| key.as_ref())
    }
    fn values(&self) -> impl Iterator<Item = &Field> {
        self.0.iter().map(|(_, value)| value)
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}
impl<'a> IntoIterator for &'a Fields {
    type Item = &'a (std::sync::Arc<str>, Field);
    type IntoIter = std::slice::Iter<'a, (std::sync::Arc<str>, Field)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
pub(super) enum Field {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(std::sync::Arc<str>),
    Node(usize),
    Array(Box<[Field]>),
    Object(Fields),
}
impl Field {
    fn as_u32(&self) -> Option<u32> {
        if let Self::Number(value) = self {
            value.as_u64().and_then(|value| value.try_into().ok())
        } else {
            None
        }
    }
}
pub(super) struct IndexedNode {
    kind: std::sync::Arc<str>,
    span: Span,
    parent: Option<usize>,
    field: Option<std::sync::Arc<str>>,
    children: Box<[usize]>,
    fields: Fields,
}

/// Owned compact AST metadata; the source text is borrowed, never modified.
pub struct QueryIndex<'a> {
    source: &'a Source,
    pub hash: String,
    pub diagnostics: Vec<Diagnostic>,
    nodes: Vec<IndexedNode>,
    root: usize,
    order: Vec<usize>,
    locations: LocationIndex,
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub max_code: usize,
    pub max_value: usize,
    pub max_items: usize,
    /// Empty means inspect all immediate fields; paths select nested fields.
    pub fields: Vec<String>,
}
impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            max_code: 500,
            max_value: 500,
            max_items: 20,
            fields: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub location: Location,
    pub parent: Option<String>,
    pub field: Option<String>,
    pub child_count: usize,
    pub code: String,
    pub code_truncated: bool,
    pub fields: BTreeMap<String, Value>,
    pub truncated_fields: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub match_count: usize,
    pub offset: usize,
    pub has_more: bool,
    pub next_offset: Option<usize>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub struct Kind {
    pub kind: String,
    pub count: usize,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Relation {
    SelfNode,
    Parent,
    Children,
    Descendants,
    Ancestors,
    Siblings,
    Context,
}

impl<'a> QueryIndex<'a> {
    pub fn parse(source: &'a Source) -> Result<Self, Error> {
        let (nodes, root, diagnostics) = {
            let allocator = Allocator::default();
            let parsed = Parser::new(
                &allocator,
                &source.code,
                SourceType::from_path(&source.name).unwrap_or(SourceType::tsx()),
            )
            .parse();
            let diagnostics = parsed
                .errors
                .iter()
                .map(|error| Diagnostic {
                    file: source.name.clone(),
                    message: error.to_string(),
                })
                .collect();
            let (nodes, root) = index::build_program(&parsed.program)?;
            (nodes, root, diagnostics)
        };
        let mut order = Vec::with_capacity(nodes.len());
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            order.push(node);
            stack.extend(nodes[node].children.iter().rev().copied());
        }
        Ok(Self {
            source,
            hash: format!("{:x}", Sha256::digest(source.code.as_bytes())),
            diagnostics,
            nodes,
            root,
            order,
            locations: LocationIndex::new(&source.code),
        })
    }

    pub fn root_id(&self) -> String {
        format!("n{}", self.root)
    }
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds: BTreeMap<&str, (usize, std::collections::BTreeSet<&str>)> = BTreeMap::new();
        for node in &self.nodes {
            let (count, fields) = kinds.entry(node.kind.as_ref()).or_default();
            *count += 1;
            fields.extend(node.fields.keys());
        }
        kinds
            .into_iter()
            .map(|(kind, (count, fields))| Kind {
                kind: kind.into(),
                count,
                fields: fields.into_iter().map(str::to_owned).collect(),
            })
            .collect()
    }

    pub fn select(
        &self,
        selector: &Selector,
        offset: usize,
        limit: usize,
        render: &RenderOptions,
    ) -> Result<Page, Error> {
        let matches = selector::evaluate(selector, self)?;
        let ids = self.order.iter().copied().filter(|id| matches[*id]);
        self.page_iter(ids, offset, limit, render)
    }

    pub fn related(
        &self,
        id: &str,
        relation: Relation,
        offset: usize,
        limit: usize,
        render: &RenderOptions,
    ) -> Result<Page, Error> {
        let id = self.id(id)?;
        let mut ids = Vec::new();
        match relation {
            Relation::SelfNode => ids.push(id),
            Relation::Parent => ids.extend(self.nodes[id].parent),
            Relation::Children => ids.extend(&self.nodes[id].children),
            Relation::Ancestors => {
                let mut current = self.nodes[id].parent;
                while let Some(id) = current {
                    ids.push(id);
                    current = self.nodes[id].parent;
                }
            }
            Relation::Descendants => {
                let mut stack: Vec<_> = self.nodes[id].children.iter().rev().copied().collect();
                while let Some(node) = stack.pop() {
                    ids.push(node);
                    stack.extend(self.nodes[node].children.iter().rev().copied());
                }
            }
            Relation::Siblings => {
                if let Some(parent) = self.nodes[id].parent {
                    ids.extend(
                        self.nodes[parent]
                            .children
                            .iter()
                            .copied()
                            .filter(|sibling| *sibling != id),
                    );
                }
            }
            Relation::Context => {
                let mut current = Some(id);
                while let Some(id) = current {
                    if matches!(
                        self.nodes[id].kind.as_ref(),
                        "Program"
                            | "FunctionDeclaration"
                            | "FunctionExpression"
                            | "ArrowFunctionExpression"
                            | "ClassDeclaration"
                            | "ClassExpression"
                            | "StaticBlock"
                    ) {
                        ids.push(id);
                        break;
                    }
                    current = self.nodes[id].parent;
                }
            }
        }
        self.page(&ids, offset, limit, render)
    }

    fn id(&self, id: &str) -> Result<usize, Error> {
        let index = id
            .strip_prefix('n')
            .and_then(|id| id.parse::<usize>().ok())
            .filter(|id| *id < self.nodes.len());
        index.ok_or_else(|| {
            Error(format!(
                "unknown node ID '{id}'; use an ID returned for this file"
            ))
        })
    }
    fn page(
        &self,
        ids: &[usize],
        offset: usize,
        limit: usize,
        options: &RenderOptions,
    ) -> Result<Page, Error> {
        self.page_iter(ids.iter().copied(), offset, limit, options)
    }
    fn page_iter(
        &self,
        ids: impl Iterator<Item = usize>,
        offset: usize,
        limit: usize,
        options: &RenderOptions,
    ) -> Result<Page, Error> {
        let mut nodes = Vec::new();
        let mut match_count = 0;
        for (position, id) in ids.enumerate() {
            if position >= offset && nodes.len() < limit {
                nodes.push(self.render(id, options)?);
            }
            match_count = position + 1;
        }
        let end = offset.saturating_add(nodes.len());
        let has_more = end < match_count;
        Ok(Page {
            match_count,
            offset,
            has_more,
            next_offset: (has_more && limit > 0).then_some(end),
            nodes,
        })
    }
    fn render(&self, id: usize, options: &RenderOptions) -> Result<Node, Error> {
        let node = &self.nodes[id];
        let code = self
            .source
            .code
            .get(node.span.start as usize..node.span.end as usize)
            .ok_or_else(|| Error("AST node has an invalid source span".into()))?;
        let (code, code_truncated) = truncate(code, options.max_code);
        let mut fields = BTreeMap::new();
        let mut truncated_fields = Vec::new();
        if options.fields.is_empty() {
            for (name, value) in &node.fields {
                fields.insert(
                    name.to_string(),
                    self.field_json(value, name, options, &mut truncated_fields),
                );
            }
        } else {
            for path in &options.fields {
                let values = self.lookup(id, path);
                let wildcard = path.split('.').any(|part| part == "*");
                let value = if values.is_empty() && !wildcard {
                    Value::Null
                } else if values.len() == 1 && !wildcard {
                    self.atom_json(&values[0], path, options, &mut truncated_fields)
                } else {
                    Value::Array(
                        values
                            .iter()
                            .take(options.max_items)
                            .map(|value| {
                                self.atom_json(value, path, options, &mut truncated_fields)
                            })
                            .collect(),
                    )
                };
                if values.len() > options.max_items && (wildcard || values.len() > 1) {
                    truncated_fields.push(path.clone());
                }
                fields.insert(path.clone(), value);
            }
        }
        truncated_fields.sort();
        truncated_fields.dedup();
        Ok(Node {
            id: format!("n{id}"),
            kind: node.kind.to_string(),
            location: self
                .locations
                .location(&self.source.code, node.span)
                .ok_or_else(|| Error("invalid AST location".into()))?,
            parent: node.parent.map(|id| format!("n{id}")),
            field: node.field.as_deref().map(str::to_owned),
            child_count: node.children.len(),
            code,
            code_truncated,
            fields,
            truncated_fields,
        })
    }

    fn field_json(
        &self,
        field: &Field,
        path: &str,
        options: &RenderOptions,
        truncated: &mut Vec<String>,
    ) -> Value {
        match field {
            Field::Node(id) => {
                json!({"node":format!("n{id}"), "kind":self.nodes[*id].kind.as_ref()})
            }
            Field::String(value) => {
                let (value, cut) = truncate(value, options.max_value);
                if cut {
                    truncated.push(path.into());
                }
                Value::String(value)
            }
            Field::Null => Value::Null,
            Field::Bool(value) => Value::Bool(*value),
            Field::Number(value) => Value::Number(value.clone()),
            Field::Array(values) => {
                if values.len() > options.max_items {
                    truncated.push(path.into());
                }
                Value::Array(
                    values
                        .iter()
                        .take(options.max_items)
                        .enumerate()
                        .map(|(index, value)| {
                            self.field_json(value, &format!("{path}.{index}"), options, truncated)
                        })
                        .collect(),
                )
            }
            Field::Object(values) => {
                if values.len() > options.max_items {
                    truncated.push(path.into());
                }
                Value::Object(
                    values
                        .iter()
                        .take(options.max_items)
                        .map(|(key, value)| {
                            (
                                key.to_string(),
                                self.field_json(
                                    value,
                                    &format!("{path}.{key}"),
                                    options,
                                    truncated,
                                ),
                            )
                        })
                        .collect(),
                )
            }
        }
    }
    fn atom_json(
        &self,
        value: &Atom<'_>,
        path: &str,
        options: &RenderOptions,
        truncated: &mut Vec<String>,
    ) -> Value {
        match value {
            Atom::Field(field) => self.field_json(field, path, options, truncated),
            Atom::Node(id) => {
                json!({"node":format!("n{id}"), "kind":self.nodes[*id].kind.as_ref()})
            }
            Atom::Scalar(value) => {
                if let Value::String(value) = value {
                    let (text, cut) = truncate(value, options.max_value);
                    if cut {
                        truncated.push(path.into());
                    }
                    Value::String(text)
                } else {
                    value.clone()
                }
            }
        }
    }
    fn lookup(&self, id: usize, path: &str) -> Vec<Atom<'_>> {
        if let Some(attribute) = path.strip_prefix('@') {
            let node = &self.nodes[id];
            let value = match attribute {
                "id" => Value::String(format!("n{id}")),
                "field" => node
                    .field
                    .as_deref()
                    .map(|value| Value::String(value.into()))
                    .unwrap_or(Value::Null),
                "text" => Value::String(
                    self.source.code[node.span.start as usize..node.span.end as usize].into(),
                ),
                "start" => json!(node.span.start),
                "end" => json!(node.span.end),
                "line" => json!(
                    self.locations
                        .location(&self.source.code, node.span)
                        .map(|loc| loc.line)
                ),
                _ => return Vec::new(),
            };
            return vec![Atom::Scalar(value)];
        }
        let mut values = vec![Atom::Node(id)];
        for part in path.split('.') {
            let mut next = Vec::new();
            for value in values {
                match value {
                    Atom::Node(_) | Atom::Field(Field::Node(_)) => {
                        let id = match value {
                            Atom::Node(id) => id,
                            Atom::Field(Field::Node(id)) => *id,
                            _ => unreachable!(),
                        };
                        if part == "start" {
                            next.push(Atom::Scalar(json!(self.nodes[id].span.start)));
                        } else if part == "end" {
                            next.push(Atom::Scalar(json!(self.nodes[id].span.end)));
                        } else if part == "*" {
                            next.extend(self.nodes[id].fields.values().map(Atom::Field));
                        } else if let Some(value) = self.nodes[id].fields.get(part) {
                            next.push(Atom::Field(value));
                        }
                    }
                    Atom::Field(Field::Array(items)) => {
                        if part == "*" {
                            next.extend(items.iter().map(Atom::Field));
                        } else if part == "length" {
                            next.push(Atom::Scalar(json!(items.len())));
                        } else if let Ok(index) = part.parse::<usize>()
                            && let Some(value) = items.get(index)
                        {
                            next.push(Atom::Field(value));
                        }
                    }
                    Atom::Field(Field::Object(fields)) => {
                        if part == "*" {
                            next.extend(fields.values().map(Atom::Field));
                        } else if let Some(value) = fields.get(part) {
                            next.push(Atom::Field(value));
                        }
                    }
                    _ => {}
                }
            }
            values = next;
        }
        values
    }
}

pub(super) enum Atom<'a> {
    Field(&'a Field),
    Node(usize),
    Scalar(Value),
}
#[derive(Clone, Copy)]
pub(super) enum Scalar<'a> {
    Null,
    Bool(bool),
    Number(&'a serde_json::Number),
    String(&'a str),
}
impl<'a> Scalar<'a> {
    fn as_str(self) -> Option<&'a str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_f64(self) -> Option<f64> {
        if let Self::Number(value) = self {
            value.as_f64()
        } else {
            None
        }
    }
    fn is_string(self) -> bool {
        matches!(self, Self::String(_))
    }
}
impl Atom<'_> {
    fn scalar(&self) -> Option<Scalar<'_>> {
        match self {
            Self::Field(Field::Null) | Self::Scalar(Value::Null) => Some(Scalar::Null),
            Self::Field(Field::Bool(value)) | Self::Scalar(Value::Bool(value)) => {
                Some(Scalar::Bool(*value))
            }
            Self::Field(Field::Number(value)) | Self::Scalar(Value::Number(value)) => {
                Some(Scalar::Number(value))
            }
            Self::Field(Field::String(value)) => Some(Scalar::String(value)),
            Self::Scalar(Value::String(value)) => Some(Scalar::String(value)),
            _ => None,
        }
    }
}
fn truncate(value: &str, limit: usize) -> (String, bool) {
    let end = value
        .char_indices()
        .nth(limit)
        .map(|(offset, _)| offset)
        .unwrap_or(value.len());
    (value[..end].into(), end < value.len())
}
