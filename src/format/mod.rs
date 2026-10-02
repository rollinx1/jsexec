//! Source formatting independent of CLI input/output. No code is executed.
mod html;

use crate::source::{Error, Source};
use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;
use oxc_span::SourceType;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default)]
pub enum InputKind {
    #[default]
    Auto,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Html,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub input_kind: InputKind,
    pub single_quote: bool,
    pub indent_width: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            input_kind: InputKind::Auto,
            single_quote: false,
            indent_width: 2,
        }
    }
}

/// Beautify JS/TS/JSX/TSX with Oxc, or HTML with html5ever.
/// JavaScript syntax errors are fatal: never emit a recovered, incomplete AST.
pub fn format(source: &Source, options: &Options) -> Result<String, Error> {
    if !(1..=16).contains(&options.indent_width) {
        return Err(Error("indent width must be between 1 and 16".into()));
    }
    let source_type = match options.input_kind {
        InputKind::Html => None,
        InputKind::JavaScript => Some(SourceType::mjs()),
        InputKind::Jsx => Some(SourceType::jsx()),
        InputKind::TypeScript => Some(SourceType::ts()),
        InputKind::Tsx => Some(SourceType::tsx()),
        InputKind::Auto => {
            let extension = Path::new(&source.name)
                .extension()
                .and_then(|ext| ext.to_str());
            if matches!(extension, Some("html" | "htm")) {
                None
            } else if source.name == "<stdin>" {
                Some(SourceType::mjs())
            } else {
                Some(SourceType::from_path(&source.name).map_err(|_| {
                    Error(format!(
                        "unknown format for '{}'; use --input-type js|jsx|ts|tsx|html",
                        source.name
                    ))
                })?)
            }
        }
    };
    if let Some(source_type) = source_type {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &source.code, source_type).parse();
        if !parsed.errors.is_empty() {
            return Err(Error(format!(
                "cannot format '{}': {}",
                source.name,
                parsed
                    .errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let mut formatted = Codegen::new()
            .with_options(CodegenOptions {
                single_quote: options.single_quote,
                indent_char: IndentChar::Space,
                indent_width: options.indent_width,
                ..CodegenOptions::default()
            })
            .build(&parsed.program)
            .code;
        if !formatted.is_empty() && !formatted.ends_with('\n') {
            formatted.push('\n');
        }
        Ok(formatted)
    } else {
        html::format(&source.code, options.indent_width)
    }
}
