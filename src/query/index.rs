use super::{Field, Fields, IndexedNode};
use crate::source::Error;
use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind, Program, Statement};
use oxc_estree::{CompactTSSerializer, ESTree};
use oxc_span::{GetSpan, Span};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

type Entries = Vec<(Arc<str>, Field)>;
enum Frame {
    Object(Entries, Option<Arc<str>>),
    Array(Vec<Field>),
}

#[derive(Default)]
struct Builder {
    nodes: Vec<IndexedNode>,
    strings: HashSet<Arc<str>>,
}
impl Builder {
    fn intern(&mut self, text: &str) -> Arc<str> {
        if let Some(value) = self.strings.get(text) {
            return value.clone();
        }
        let value: Arc<str> = text.into();
        self.strings.insert(value.clone());
        value
    }
    fn scalar_value(&mut self, value: Value) -> Field {
        match value {
            Value::Null => Field::Null,
            Value::Bool(value) => Field::Bool(value),
            Value::Number(value) => Field::Number(value),
            Value::String(value) => Field::String(self.intern(&value)),
            _ => unreachable!("only scalar values are decoded here"),
        }
    }
    fn scalar(&mut self, text: &str) -> Result<(Field, usize), Error> {
        if text.starts_with('"') {
            let mut escaped = false;
            let end = text
                .bytes()
                .enumerate()
                .skip(1)
                .find_map(|(index, byte)| {
                    if byte == b'"' && !escaped {
                        return Some(index + 1);
                    }
                    escaped = byte == b'\\' && !escaped;
                    None
                })
                .ok_or_else(|| Error("unterminated ESTree string".into()))?;
            let quoted = &text[..end];
            if !quoted.contains('\\') {
                return Ok((
                    Field::String(self.intern(&quoted[1..quoted.len() - 1])),
                    end,
                ));
            }
            let field = match serde_json::from_str::<String>(quoted) {
                Ok(value) => Field::String(self.intern(&value)),
                Err(_) => {
                    let units = utf16(quoted)?
                        .into_iter()
                        .map(|unit| Field::Number(unit.into()))
                        .collect::<Vec<_>>()
                        .into_boxed_slice();
                    let encoding = self.intern("encoding");
                    let value = self.intern("utf16");
                    let key = self.intern("units");
                    Field::Object(Fields(
                        vec![(encoding, Field::String(value)), (key, Field::Array(units))]
                            .into_boxed_slice(),
                    ))
                }
            };
            return Ok((field, end));
        }
        let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
        let value = stream
            .next()
            .ok_or_else(|| Error("missing ESTree value".into()))?
            .map_err(|error| Error(format!("cannot decode ESTree field: {error}")))?;
        Ok((self.scalar_value(value), stream.byte_offset()))
    }
    // Iterative fragment decoding preserves deep trees and post-order node IDs.
    fn fragment(&mut self, json: &str) -> Result<Field, Error> {
        let mut frames = Vec::new();
        let mut root = None;
        let mut offset = 0;
        while offset < json.len() {
            match json.as_bytes()[offset] {
                b'{' => {
                    frames.push(Frame::Object(Vec::new(), None));
                    offset += 1;
                }
                b'[' => {
                    frames.push(Frame::Array(Vec::new()));
                    offset += 1;
                }
                b'}' | b']' => {
                    let frame = frames
                        .pop()
                        .ok_or_else(|| Error("invalid ESTree serialization".into()))?;
                    let value = match frame {
                        Frame::Array(values) => Field::Array(values.into_boxed_slice()),
                        Frame::Object(fields, _) => self.object(fields),
                    };
                    append(value, &mut frames, &mut root)?;
                    offset += 1;
                }
                b',' | b':' | b' ' | b'\n' | b'\r' | b'\t' => offset += 1,
                _ => {
                    let (scalar, consumed) = self.scalar(&json[offset..])?;
                    offset += consumed;
                    if let Some(Frame::Object(_, key @ None)) = frames.last_mut() {
                        let Field::String(value) = scalar else {
                            return Err(Error("invalid ESTree field name".into()));
                        };
                        *key = Some(value);
                    } else {
                        append(scalar, &mut frames, &mut root)?;
                    }
                }
            }
        }
        root.ok_or_else(|| Error("missing ESTree root".into()))
    }
    fn serialize(&mut self, value: &impl ESTree) -> Result<Field, Error> {
        let mut serializer = CompactTSSerializer::new(false);
        value.serialize(&mut serializer);
        self.fragment(&serializer.into_string())
    }
    fn object(&mut self, mut entries: Entries) -> Field {
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut fields = Fields(entries.into_boxed_slice());
        let kind = match fields.get("type") {
            Some(Field::String(kind)) => Some(kind.clone()),
            _ => None,
        };
        let start = fields.get("start").and_then(Field::as_u32);
        let end = fields.get("end").and_then(Field::as_u32);
        if let (Some(kind), Some(start), Some(end)) = (kind, start, end) {
            fields = Fields(
                fields
                    .0
                    .into_vec()
                    .into_iter()
                    .filter(|(key, _)| !matches!(key.as_ref(), "start" | "end"))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            );
            let id = self.nodes.len();
            let mut children = Vec::new();
            for (field, value) in &fields {
                self.collect_children(value, field, &mut children);
            }
            for (child, field) in &children {
                self.nodes[*child].parent = Some(id);
                self.nodes[*child].field = Some(field.clone());
            }
            children.sort_by_key(|(child, _)| {
                (
                    self.nodes[*child].span.start,
                    self.nodes[*child].span.end,
                    *child,
                )
            });
            self.nodes.push(IndexedNode {
                kind,
                span: Span::new(start, end),
                parent: None,
                field: None,
                children: children
                    .into_iter()
                    .map(|(child, _)| child)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                fields,
            });
            Field::Node(id)
        } else {
            Field::Object(fields)
        }
    }
    fn collect_children(
        &mut self,
        value: &Field,
        path: &str,
        children: &mut Vec<(usize, Arc<str>)>,
    ) {
        match value {
            Field::Node(node) => children.push((*node, self.intern(path))),
            Field::Array(values) => {
                for (i, value) in values.iter().enumerate() {
                    self.collect_children(value, &format!("{path}.{i}"), children);
                }
            }
            Field::Object(values) => {
                for (field, value) in values {
                    self.collect_children(value, &format!("{path}.{field}"), children);
                }
            }
            _ => {}
        }
    }
}

/// Serialize one top-level subtree at a time, retaining Oxc's complete ESTree schema.
/// Oxc's serializer has a sealed buffer implementation; individual subtrees still
/// require JSON, but there is no whole-file JSON buffer alongside the index.
pub(super) fn build_program(program: &Program<'_>) -> Result<(Vec<IndexedNode>, usize), Error> {
    let mut builder = Builder::default();
    let mut body = Vec::with_capacity(program.directives.len() + program.body.len());
    for directive in &program.directives {
        body.push(builder.serialize(directive)?);
    }
    for statement in &program.body {
        body.push(builder.serialize(statement)?);
    }
    let hashbang = builder.serialize(&program.hashbang)?;
    // Keep ProgramConverter's TS span rules in sync with the pinned Oxc version.
    let start = if let Some(directive) = program.directives.first() {
        directive.span.start
    } else if let Some(statement) = program.body.first() {
        let start = statement.span().start;
        let decorator = match statement {
            Statement::ExportNamedDeclaration(decl) => match &decl.declaration {
                Some(Declaration::ClassDeclaration(class)) => class.decorators.first(),
                _ => None,
            },
            Statement::ExportDefaultDeclaration(decl) => match &decl.declaration {
                ExportDefaultDeclarationKind::ClassDeclaration(class) => class.decorators.first(),
                _ => None,
            },
            _ => None,
        };
        decorator.map_or(start, |decorator| start.min(decorator.span.start))
    } else {
        program.span.end
    };
    let fields = vec![
        (
            builder.intern("type"),
            Field::String(builder.intern("Program")),
        ),
        (
            builder.intern("body"),
            Field::Array(body.into_boxed_slice()),
        ),
        (
            builder.intern("sourceType"),
            Field::String(builder.intern(if program.source_type.is_module() {
                "module"
            } else {
                "script"
            })),
        ),
        (builder.intern("hashbang"), hashbang),
        (builder.intern("start"), Field::Number(start.into())),
        (
            builder.intern("end"),
            Field::Number(program.span.end.into()),
        ),
    ];
    let Field::Node(root) = builder.object(fields) else {
        return Err(Error("ESTree root is not a node".into()));
    };
    Ok((builder.nodes, root))
}

fn utf16(quoted: &str) -> Result<Vec<u16>, Error> {
    let mut chars = quoted[1..quoted.len() - 1].chars();
    let mut units = Vec::new();
    while let Some(mut ch) = chars.next() {
        if ch == '\\' {
            ch = match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return Err(Error("invalid ESTree Unicode escape".into()));
                    }
                    units.push(
                        u16::from_str_radix(&hex, 16)
                            .map_err(|_| Error("invalid ESTree Unicode escape".into()))?,
                    );
                    continue;
                }
                Some('"') => '"',
                Some('\\') => '\\',
                Some('/') => '/',
                Some('b') => '\u{8}',
                Some('f') => '\u{c}',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                _ => return Err(Error("invalid ESTree string escape".into())),
            };
        }
        units.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
    Ok(units)
}

fn append(value: Field, frames: &mut [Frame], root: &mut Option<Field>) -> Result<(), Error> {
    match frames.last_mut() {
        Some(Frame::Array(values)) => values.push(value),
        Some(Frame::Object(fields, key)) => {
            let key = key
                .take()
                .ok_or_else(|| Error("missing ESTree field name".into()))?;
            if let Some((_, existing)) = fields.iter_mut().find(|(name, _)| *name == key) {
                *existing = value;
            } else {
                fields.push((key, value));
            }
        }
        None => *root = Some(value),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;
    use serde_json::json;

    fn field(value: &Field) -> Value {
        match value {
            Field::Null => Value::Null,
            Field::Bool(value) => json!(value),
            Field::Number(value) => json!(value),
            Field::String(value) => json!(value.as_ref()),
            Field::Node(value) => json!({"node": value}),
            Field::Array(values) => Value::Array(values.iter().map(field).collect()),
            Field::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.to_string(), field(value)))
                    .collect(),
            ),
        }
    }
    fn snapshot(nodes: &[IndexedNode]) -> Value {
        Value::Array(nodes.iter().map(|node| json!({
            "kind": node.kind.as_ref(), "start": node.span.start, "end": node.span.end,
            "parent": node.parent, "field": node.field.as_deref(), "children": node.children,
            "fields": node.fields.iter().map(|(key, value)| (key.to_string(), field(value))).collect::<serde_json::Map<_,_>>()
        })).collect())
    }

    #[test]
    fn fragment_index_preserves_complete_oxc_schema_ids_and_program_spans() {
        for (name, code) in [
            ("empty.js", ""),
            ("comment.js", "/* comment */"),
            ("hashbang.js", "#!/usr/bin/env node\n"),
            (
                "directive.js",
                "#!/usr/bin/env node\n\"use strict\";\nfoo();",
            ),
            ("common.cjs", "module.exports = function(){ return 42; };"),
            ("example.ts", include_str!("../../examples/query.ts")),
            ("decorator.ts", "@decorate export class First {}"),
            ("default.ts", "@decorate export default class First {}"),
            (
                "features.tsx",
                r#"const {a: b, ...rest} = x; const a = <Widget value={foo?.bar} />; const s = "a\uD800"; const r=/a/i; const n=123n;"#,
            ),
            ("broken.js", "const = ;"),
        ] {
            let allocator = Allocator::default();
            let parsed =
                Parser::new(&allocator, code, SourceType::from_path(name).unwrap()).parse();
            let json = parsed.program.to_estree_ts_json(false);
            let mut legacy = Builder::default();
            let Field::Node(legacy_root) = legacy.fragment(&json).unwrap() else {
                panic!("root")
            };
            let (nodes, root) = build_program(&parsed.program).unwrap();
            assert_eq!(root, legacy_root, "{name}");
            assert_eq!(snapshot(&nodes), snapshot(&legacy.nodes), "{name}");
        }
    }
}
