use super::{Candidate, ChunkExtractor, Context, InputKind};
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde_json::Value;

pub struct Manifest;
pub struct Html;

impl ChunkExtractor for Manifest {
    fn name(&self) -> &'static str {
        "manifest"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        if context.kind == InputKind::Json {
            collect_json(&context.source.code, candidates);
        } else if context.kind == InputKind::Html {
            visit_document(&context.source.code, |node| {
                if let NodeData::Element { name, attrs, .. } = &node.data
                    && name.local.as_ref() == "script"
                    && attrs.borrow().iter().any(|attr| {
                        attr.name.local.as_ref() == "type"
                            && attr.value.split(';').next().is_some_and(|mime| {
                                mime.trim().eq_ignore_ascii_case("application/json")
                            })
                    })
                {
                    let mut text = String::new();
                    for child in node.children.borrow().iter() {
                        if let NodeData::Text { contents } = &child.data {
                            text.push_str(&contents.borrow());
                        }
                    }
                    collect_json(&text, candidates);
                }
            });
        }
    }
}

fn collect_json(source: &str, candidates: &mut Vec<Candidate>) {
    let Ok(value) = serde_json::from_str::<Value>(source) else {
        return;
    };
    let mut stack = vec![&value];
    while let Some(value) = stack.pop() {
        match value {
            Value::String(reference) => candidates.push(Candidate::new(reference.clone(), None)),
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => stack.extend(values.values()),
            _ => {}
        }
    }
}

impl ChunkExtractor for Html {
    fn name(&self) -> &'static str {
        "html"
    }
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>) {
        if context.kind != InputKind::Html {
            return;
        }
        visit_document(&context.source.code, |node| {
            if let NodeData::Element { name, attrs, .. } = &node.data {
                let attrs = attrs.borrow();
                let attr = |key: &str| {
                    attrs
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == key)
                        .map(|attr| attr.value.to_string())
                };
                let reference = match name.local.as_ref() {
                    "script" => attr("src"),
                    "link" => {
                        let rel = attr("rel").unwrap_or_default();
                        if rel
                            .split_ascii_whitespace()
                            .any(|rel| rel.eq_ignore_ascii_case("modulepreload"))
                            || (rel
                                .split_ascii_whitespace()
                                .any(|rel| rel.eq_ignore_ascii_case("preload"))
                                && attr("as")
                                    .is_some_and(|value| value.eq_ignore_ascii_case("script")))
                        {
                            attr("href")
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(reference) = reference {
                    candidates.push(Candidate::new(reference, None));
                }
            }
        });
    }
}

fn visit_document(source: &str, mut visitor: impl FnMut(&Handle)) {
    let document = parse_document(RcDom::default(), Default::default()).one(source);
    // Keep the root alive: RcDom's Node::drop drains descendant child lists.
    let mut stack = vec![document.document.clone()];
    while let Some(node) = stack.pop() {
        visitor(&node);
        stack.extend(node.children.borrow().iter().cloned());
    }
}
