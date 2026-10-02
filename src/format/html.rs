use crate::source::Error;
use html5ever::serialize::{HtmlSerializer, SerializeOpts, Serializer};
use html5ever::tendril::TendrilSink;
use html5ever::{QualName, parse_document};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

enum Event {
    Node(Handle, usize, bool),
    Close(QualName),
    Whitespace(usize),
}

/// Use the browser parser and its serializer for escaping, namespaces, and void
/// tags. Indent structural containers only; mixed/inline and raw text stay intact.
pub(super) fn format(code: &str, width: usize) -> Result<String, Error> {
    let dom = parse_document(RcDom::default(), Default::default()).one(code);
    // RcDom's node destructor clears descendant children even when handles are
    // shared. Keep the owning document alive throughout traversal.
    let bytes = render(dom.document.clone(), width)
        .map_err(|error| Error(format!("cannot serialize HTML: {error}")))?;
    String::from_utf8(bytes)
        .map_err(|error| Error(format!("cannot encode formatted HTML: {error}")))
}

fn render(document: Handle, width: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut serializer = HtmlSerializer::new(&mut bytes, SerializeOpts::default());
        let mut events = vec![Event::Node(document, 0, true)];
        while let Some(event) = events.pop() {
            match event {
                Event::Whitespace(depth) => {
                    serializer.write_text(&format!("\n{}", " ".repeat(depth * width)))?
                }
                Event::Close(name) => serializer.end_elem(name)?,
                Event::Node(node, depth, allow_layout) => match &node.data {
                    NodeData::Document => enqueue(children(&node), depth, true, false, &mut events),
                    NodeData::Element { name, attrs, .. } => {
                        if name.local.as_ref() == "plaintext"
                            && name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                        {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "cannot format HTML containing <plaintext>; serializing it changes its content",
                            ));
                        }
                        serializer.start_elem(
                            name.clone(),
                            attrs
                                .borrow()
                                .iter()
                                .map(|attr| (&attr.name, attr.value.as_ref())),
                        )?;
                        let descendants = children(&node);
                        let allow_layout = allow_layout
                            && name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                            && !matches!(
                                name.local.as_ref(),
                                "pre"
                                    | "listing"
                                    | "textarea"
                                    | "script"
                                    | "style"
                                    | "xmp"
                                    | "iframe"
                                    | "noembed"
                                    | "noframes"
                                    | "plaintext"
                                    | "noscript"
                                    | "title"
                            );
                        // The HTML parser consumes one initial newline in these
                        // elements. Add it back so re-parsing retains the text node.
                        if name.ns.as_ref() == "http://www.w3.org/1999/xhtml" && matches!(name.local.as_ref(), "pre" | "textarea" | "listing")
                            && descendants.first().is_some_and(|node| matches!(&node.data, NodeData::Text { contents } if contents.borrow().starts_with('\n')))
                        { serializer.write_text("\n")?; }
                        let layout = allow_layout
                            && container(name.local.as_ref())
                            && descendants.iter().all(structural)
                            // Whitespace after </body> is parsed into the body. A
                            // mixed-content body cannot normalize inserted layout,
                            // so keep its surrounding html container compact too.
                            && (name.local.as_ref() != "html" || descendants.iter().all(|child| {
                                !matches!(&child.data, NodeData::Element { name, .. } if name.local.as_ref() == "body") || children(child).iter().all(structural)
                            }));
                        events.push(Event::Close(name.clone()));
                        if layout && descendants.iter().any(|node| !whitespace(node)) {
                            events.push(Event::Whitespace(depth));
                        }
                        enqueue(descendants, depth + 1, allow_layout, layout, &mut events);
                    }
                    NodeData::Text { contents } => serializer.write_text(&contents.borrow())?,
                    NodeData::Comment { contents } => serializer.write_comment(contents)?,
                    NodeData::Doctype {
                        name,
                        public_id,
                        system_id,
                    } => {
                        // Serializer's API accepts the doctype body; retain legacy IDs
                        // because dropping them can change the document's quirks mode.
                        let body = if !public_id.is_empty() {
                            let mut body = format!("{name} PUBLIC {}", quoted(public_id)?);
                            if !system_id.is_empty() {
                                body.push_str(&format!(" {}", quoted(system_id)?));
                            }
                            body
                        } else if !system_id.is_empty() {
                            format!("{name} SYSTEM {}", quoted(system_id)?)
                        } else {
                            name.to_string()
                        };
                        serializer.write_doctype(&body)?;
                        serializer.write_text("\n")?;
                    }
                    NodeData::ProcessingInstruction { target, contents } => {
                        serializer.write_processing_instruction(target, contents)?
                    }
                },
            }
        }
    }
    Ok(bytes)
}

fn children(node: &Handle) -> Vec<Handle> {
    if let NodeData::Element {
        template_contents, ..
    } = &node.data
        && let Some(contents) = template_contents.borrow().as_ref()
    {
        return contents.children.borrow().clone();
    }
    node.children.borrow().clone()
}
fn whitespace(node: &Handle) -> bool {
    matches!(&node.data, NodeData::Text { contents } if contents.borrow().chars().all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}')))
}
fn structural(node: &Handle) -> bool {
    whitespace(node)
        || matches!(&node.data, NodeData::Comment { .. })
        || matches!(&node.data, NodeData::Element { name, .. } if name.ns.as_ref() == "http://www.w3.org/1999/xhtml" && block(name.local.as_ref()))
}
fn container(tag: &str) -> bool {
    matches!(
        tag,
        "html"
            | "head"
            | "body"
            | "main"
            | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "form"
            | "fieldset"
            | "template"
    )
}
fn block(tag: &str) -> bool {
    container(tag)
        || matches!(
            tag,
            "p" | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "pre"
                | "blockquote"
                | "dl"
                | "dt"
                | "dd"
                | "hr"
                | "script"
                | "style"
                | "title"
                | "meta"
                | "link"
                | "base"
        )
}
fn enqueue(
    nodes: Vec<Handle>,
    depth: usize,
    allow_layout: bool,
    layout: bool,
    events: &mut Vec<Event>,
) {
    let nodes: Vec<_> = nodes
        .into_iter()
        .filter(|node| !layout || !whitespace(node))
        .collect();
    for (index, node) in nodes.into_iter().enumerate().rev() {
        events.push(Event::Node(node, depth, allow_layout));
        if layout && (depth > 0 || index > 0) {
            events.push(Event::Whitespace(depth));
        }
    }
}
fn quoted(value: &str) -> std::io::Result<String> {
    if !value.contains('"') {
        Ok(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Ok(format!("'{value}'"))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cannot serialize an HTML doctype ID containing both quote styles",
        ))
    }
}
