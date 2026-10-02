use oxc_span::Span;
use serde::Serialize;
use std::fmt;

/// A decoded source file; names are used for evidence and source-type inference.
pub struct Source {
    pub name: String,
    pub code: String,
}

#[derive(Debug)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Location {
    pub start: u32,
    pub end: u32,
    /// One-based line and Unicode character column.
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub file: String,
    pub message: String,
}

// Index once rather than rescanning a multi-megabyte minified bundle per finding.
pub(crate) struct LocationIndex {
    lines: Vec<usize>,
    continuation_bytes: Vec<usize>,
}
impl LocationIndex {
    pub(crate) fn new(code: &str) -> Self {
        let mut index = Self {
            lines: vec![0],
            continuation_bytes: Vec::new(),
        };
        for (offset, byte) in code.bytes().enumerate() {
            if byte == b'\n' {
                index.lines.push(offset + 1);
            }
            if byte & 0xc0 == 0x80 {
                index.continuation_bytes.push(offset);
            }
        }
        index
    }
    pub(crate) fn location(&self, code: &str, span: Span) -> Option<Location> {
        let start = span.start as usize;
        code.get(start..span.end as usize)?;
        let line = self.lines.partition_point(|offset| *offset <= start);
        let line_start = self.lines[line - 1];
        let extra_bytes = self
            .continuation_bytes
            .partition_point(|offset| *offset < start)
            - self
                .continuation_bytes
                .partition_point(|offset| *offset < line_start);
        Some(Location {
            start: span.start,
            end: span.end,
            line,
            column: start - line_start - extra_bytes + 1,
        })
    }
}
