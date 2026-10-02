mod assets;
mod javascript;
mod manifest;
mod webpack;

use crate::source::LocationIndex;
pub use crate::source::{Diagnostic, Error, Location, Source};
use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Auto,
    JavaScript,
    Json,
    Html,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub base_url: Option<Url>,
    /// Empty means all registered extractors. Unknown names are errors.
    pub extractors: BTreeSet<String>,
    pub input_kind: InputKind,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base_url: None,
            extractors: BTreeSet::new(),
            input_kind: InputKind::Auto,
        }
    }
}

/// One extractor's raw finding. Spans are byte offsets into the source, when known.
pub struct Candidate {
    pub reference: String,
    pub span: Option<Span>,
}
impl Candidate {
    pub fn new(reference: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            reference: reference.into(),
            span,
        }
    }
}

/// JavaScript is parsed once and shared by every selected extractor.
pub struct Context<'a> {
    pub source: &'a Source,
    pub kind: InputKind,
    pub program: Option<&'a Program<'a>>,
}

/// Implement this trait and register it to add a detector without changing the CLI.
pub trait ChunkExtractor {
    fn name(&self) -> &'static str;
    fn extract(&self, context: &Context<'_>, candidates: &mut Vec<Candidate>);
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Evidence {
    pub file: String,
    pub extractor: String,
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

#[derive(Debug, Serialize)]
pub struct Chunk {
    /// Resolved URL when a base was supplied, otherwise the original reference.
    pub value: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub chunks: Vec<Chunk>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Engine {
    extractors: Vec<Box<dyn ChunkExtractor>>,
}
impl Default for Engine {
    fn default() -> Self {
        let mut engine = Self::new();
        engine.register(javascript::Imports);
        engine.register(javascript::References);
        engine.register(javascript::Vite);
        engine.register(webpack::Webpack);
        engine.register(manifest::Manifest);
        engine.register(manifest::Html);
        engine
    }
}

impl Engine {
    /// An empty registry for library consumers supplying their own extractors.
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }
    pub fn register(&mut self, extractor: impl ChunkExtractor + 'static) {
        self.extractors.push(Box::new(extractor));
    }
    pub fn extractor_names(&self) -> Vec<&'static str> {
        self.extractors
            .iter()
            .map(|extractor| extractor.name())
            .collect()
    }

    pub fn analyze(&self, sources: &[Source], options: &Options) -> Result<Report, Error> {
        let names = self.extractor_names();
        for name in &options.extractors {
            if !names.contains(&name.as_str()) {
                return Err(Error(format!(
                    "unknown extractor '{name}'; choose from {}",
                    names.join(", ")
                )));
            }
        }
        if let Some(base) = &options.base_url
            && (!matches!(base.scheme(), "http" | "https") || base.host_str().is_none())
        {
            return Err(Error("base URL must be an absolute HTTP(S) URL".into()));
        }
        let mut report = Report::default();
        let mut grouped: BTreeMap<String, BTreeSet<Evidence>> = BTreeMap::new();
        for source in sources {
            let kind = input_kind(source, options.input_kind);
            let locations = LocationIndex::new(&source.code);
            let allocator = Allocator::default();
            let parsed = (kind == InputKind::JavaScript).then(|| {
                let source_type = SourceType::from_path(&source.name).unwrap_or(SourceType::tsx());
                Parser::new(&allocator, &source.code, source_type).parse()
            });
            if let Some(parsed) = &parsed {
                for error in &parsed.errors {
                    report.diagnostics.push(Diagnostic {
                        file: source.name.clone(),
                        message: error.to_string(),
                    });
                }
            } else if kind == InputKind::Json
                && let Err(error) = serde_json::from_str::<serde_json::Value>(&source.code)
            {
                report.diagnostics.push(Diagnostic {
                    file: source.name.clone(),
                    message: error.to_string(),
                });
            }
            // Keep recovered AST findings alongside syntax diagnostics.
            let context = Context {
                source,
                kind,
                program: parsed.as_ref().map(|parsed| &parsed.program),
            };
            for extractor in &self.extractors {
                if !options.extractors.is_empty() && !options.extractors.contains(extractor.name())
                {
                    continue;
                }
                let mut candidates = Vec::new();
                extractor.extract(&context, &mut candidates);
                for candidate in candidates {
                    let reference = candidate.reference.trim();
                    if !assets::is_javascript_asset(reference) {
                        continue;
                    }
                    let value = match &options.base_url {
                        Some(base) => match base.join(reference) {
                            Ok(url) => url.to_string(),
                            Err(error) => {
                                report.diagnostics.push(Diagnostic {
                                    file: source.name.clone(),
                                    message: format!("cannot resolve '{reference}': {error}"),
                                });
                                continue;
                            }
                        },
                        None => reference.to_string(),
                    };
                    grouped.entry(value).or_default().insert(Evidence {
                        file: source.name.clone(),
                        extractor: extractor.name().into(),
                        reference: reference.into(),
                        location: candidate
                            .span
                            .and_then(|span| locations.location(&source.code, span)),
                    });
                }
            }
        }
        report.chunks = grouped
            .into_iter()
            .map(|(value, evidence)| Chunk {
                value,
                evidence: evidence.into_iter().collect(),
            })
            .collect();
        Ok(report)
    }
}

fn input_kind(source: &Source, requested: InputKind) -> InputKind {
    if requested != InputKind::Auto {
        return requested;
    }
    let name = source.name.to_ascii_lowercase();
    if name.ends_with(".jsx") || name.ends_with(".tsx") {
        InputKind::JavaScript
    } else if name.ends_with(".html")
        || name.ends_with(".htm")
        || source.code.trim_start().starts_with('<')
    {
        InputKind::Html
    } else if name.ends_with(".json")
        || serde_json::from_str::<serde_json::Value>(&source.code).is_ok()
    {
        InputKind::Json
    } else {
        InputKind::JavaScript
    }
}
