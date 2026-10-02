use super::{Field, IndexedNode};
use crate::source::Error;
use serde_json::Value;
use std::collections::BTreeMap;

enum Frame {
    Object(BTreeMap<String, Field>, Option<String>),
    Array(Vec<Field>),
}

/// Consume the ESTree serialization iteratively. Child trees become arena references
/// immediately, so there is no second full JSON AST and no JSON recursion limit.
pub(super) fn build(json: &str) -> Result<(Vec<IndexedNode>, usize), Error> {
    let mut nodes: Vec<IndexedNode> = Vec::new();
    let mut frames = Vec::new();
    let mut root = None;
    let mut offset = 0;
    while offset < json.len() {
        let byte = json.as_bytes()[offset];
        match byte {
            b'{' => {
                frames.push(Frame::Object(BTreeMap::new(), None));
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
                    Frame::Array(values) => Field::Array(values),
                    Frame::Object(mut fields, _) => {
                        let kind = fields.get("type").and_then(Field::as_str);
                        let start = fields.get("start").and_then(Field::as_u32);
                        let end = fields.get("end").and_then(Field::as_u32);
                        if let (Some(kind), Some(start), Some(end)) = (kind, start, end) {
                            let kind = kind.to_string();
                            fields.remove("start");
                            fields.remove("end");
                            let id = nodes.len();
                            let mut children = Vec::new();
                            for (field, value) in &fields {
                                collect_children(value, field, &mut children);
                            }
                            for (child, field) in &children {
                                nodes[*child].parent = Some(id);
                                nodes[*child].field = Some(field.clone());
                            }
                            children.sort_by_key(|(child, _)| {
                                (nodes[*child].span.start, nodes[*child].span.end, *child)
                            });
                            nodes.push(IndexedNode {
                                kind,
                                span: oxc_span::Span::new(start, end),
                                parent: None,
                                field: None,
                                children: children.into_iter().map(|(child, _)| child).collect(),
                                fields,
                            });
                            Field::Node(id)
                        } else {
                            Field::Object(fields)
                        }
                    }
                };
                append(value, &mut frames, &mut root)?;
                offset += 1;
            }
            b',' | b':' | b' ' | b'\n' | b'\r' | b'\t' => offset += 1,
            _ => {
                let (scalar, consumed) = scalar(&json[offset..])?;
                offset += consumed;
                if let Some(Frame::Object(_, key @ None)) = frames.last_mut() {
                    *key = Some(
                        scalar
                            .as_str()
                            .ok_or_else(|| Error("invalid ESTree field name".into()))?
                            .into(),
                    );
                } else {
                    append(scalar, &mut frames, &mut root)?;
                }
            }
        }
    }
    let Some(Field::Node(root)) = root else {
        return Err(Error("ESTree root is not a node".into()));
    };
    Ok((nodes, root))
}

fn scalar(text: &str) -> Result<(Field, usize), Error> {
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
        let field = match serde_json::from_str::<String>(quoted) {
            Ok(value) => Field::Scalar(Value::String(value)),
            // JavaScript strings can contain unpaired UTF-16 surrogates. Preserve
            // their units explicitly instead of rejecting the entire AST or losing data.
            Err(_) => Field::Object(BTreeMap::from([
                (
                    "encoding".into(),
                    Field::Scalar(Value::String("utf16".into())),
                ),
                (
                    "units".into(),
                    Field::Array(
                        utf16(quoted)?
                            .into_iter()
                            .map(|unit| Field::Scalar(Value::from(unit)))
                            .collect(),
                    ),
                ),
            ])),
        };
        return Ok((field, end));
    }
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = stream
        .next()
        .ok_or_else(|| Error("missing ESTree value".into()))?
        .map_err(|error| Error(format!("cannot decode ESTree field: {error}")))?;
    Ok((Field::Scalar(value), stream.byte_offset()))
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
            fields.insert(key, value);
        }
        None => *root = Some(value),
    }
    Ok(())
}

fn collect_children(value: &Field, path: &str, children: &mut Vec<(usize, String)>) {
    match value {
        Field::Node(node) => children.push((*node, path.into())),
        Field::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_children(value, &format!("{path}.{index}"), children);
            }
        }
        Field::Object(values) => {
            for (field, value) in values {
                collect_children(value, &format!("{path}.{field}"), children);
            }
        }
        Field::Scalar(_) => {}
    }
}
