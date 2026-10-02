mod data_uri;
mod parser;

use crate::source::{Diagnostic, Error, Location, LocationIndex, Source};
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde::Serialize;
use url::Url;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputKind {
    #[default]
    Auto,
    JavaScript,
    Map,
}

#[derive(Debug, Default)]
pub struct Options {
    /// Original generated-script URL for JavaScript, or original map URL for a map.
    pub base_url: Option<Url>,
    pub input_kind: InputKind,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MapKind {
    File,
    Inline,
    External,
}

#[derive(Debug, Serialize)]
pub struct SourceEntry {
    /// Index within this section's original sources array, including null entries.
    pub index: usize,
    pub section: Vec<usize>,
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_url: Option<String>,
    /// None means missing content; an empty string is a recovered empty file.
    pub content: Option<String>,
    pub ignored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SectionReference {
    pub section: Vec<usize>,
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SourceMap {
    pub input: String,
    pub kind: MapKind,
    /// External annotation only. Inline data payloads are not duplicated in reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_file: Option<String>,
    pub sources: Vec<SourceEntry>,
    pub external_sections: Vec<SectionReference>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub maps: Vec<SourceMap>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn embedded_count(&self) -> usize {
        self.maps
            .iter()
            .flat_map(|map| &map.sources)
            .filter(|source| source.content.is_some())
            .count()
    }
    pub(crate) fn diagnostic(&mut self, input: &str, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            file: input.into(),
            message: message.into(),
        });
    }
}

/// Discover annotations and recover embedded sources without reading files or fetching URLs.
pub fn analyze(sources: &[Source], options: &Options) -> Result<Report, Error> {
    if let Some(base) = &options.base_url
        && (!matches!(base.scheme(), "http" | "https") || base.host_str().is_none())
    {
        return Err(Error("base URL must be an absolute HTTP(S) URL".into()));
    }
    let mut report = Report::default();
    for source in sources {
        let is_map = match options.input_kind {
            InputKind::Map => true,
            InputKind::JavaScript => false,
            InputKind::Auto => {
                let name = source.name.to_ascii_lowercase();
                let extension = std::path::Path::new(&name)
                    .extension()
                    .and_then(|extension| extension.to_str());
                !matches!(extension, Some("js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx"))
                    && (name.ends_with(".map")
                        || name.ends_with(".json")
                        || source
                            .code
                            .trim_start_matches('\u{feff}')
                            .trim_start()
                            .starts_with('{'))
            }
        };
        if is_map {
            let mut map = new_map(source, MapKind::File, options.base_url.clone(), None);
            parser::parse(&source.code, &mut map, &mut report);
            report.maps.push(map);
            continue;
        }
        let allocator = Allocator::default();
        let parsed = Parser::new(
            &allocator,
            &source.code,
            SourceType::from_path(&source.name).unwrap_or(SourceType::tsx()),
        )
        .parse();
        for error in &parsed.errors {
            report.diagnostic(&source.name, error.to_string());
        }
        // Match actual comments, not sourceMappingURL text inside strings/templates/regexes.
        // Like backendcaido, the final matching annotation is authoritative.
        let annotation = parsed
            .program
            .comments
            .iter()
            .filter_map(|comment| {
                let span = comment.content_span();
                let text = source.code.get(span.start as usize..span.end as usize)?;
                Some((annotation_url(text)?, comment.span))
            })
            .next_back();
        let Some((reference, span)) = annotation else {
            continue;
        };
        if reference.is_empty() {
            continue;
        }
        let location = LocationIndex::new(&source.code).location(&source.code, span);
        if reference
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        {
            let mut map = new_map(source, MapKind::Inline, options.base_url.clone(), location);
            match data_uri::decode(reference) {
                Ok(json) => parser::parse(&json, &mut map, &mut report),
                Err(error) => report.diagnostic(&source.name, error.to_string()),
            }
            report.maps.push(map);
        } else {
            let url = resolve_map_url(reference, options.base_url.as_ref());
            if let Err(error) = &url {
                report.diagnostic(&source.name, error.to_string());
            }
            let mut map = new_map(source, MapKind::External, url.ok().flatten(), location);
            map.reference = Some(reference.into());
            report.maps.push(map);
        }
    }
    Ok(report)
}

fn new_map(
    source: &Source,
    kind: MapKind,
    url: Option<Url>,
    location: Option<Location>,
) -> SourceMap {
    SourceMap {
        input: source.name.clone(),
        kind,
        reference: None,
        url: url.map(|url| url.to_string()),
        location,
        generated_file: None,
        sources: Vec::new(),
        external_sections: Vec::new(),
    }
}

fn annotation_url(comment: &str) -> Option<&str> {
    let rest = comment
        .strip_prefix('#')
        .or_else(|| comment.strip_prefix('@'))?
        .trim_start();
    let url = rest.strip_prefix("sourceMappingURL=")?.trim_end();
    (!url.chars().any(char::is_whitespace)).then_some(url)
}

pub(super) fn resolve_map_url(reference: &str, base: Option<&Url>) -> Result<Option<Url>, Error> {
    if reference.is_empty() {
        return Err(Error("source map reference is empty".into()));
    }
    if reference.contains('\\')
        || reference
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(Error(format!("invalid source map reference '{reference}'")));
    }
    let url = match Url::parse(reference) {
        Ok(url) => Some(url),
        Err(url::ParseError::RelativeUrlWithoutBase) => match base {
            Some(base) => Some(
                base.join(reference)
                    .map_err(|error| Error(format!("cannot resolve map reference: {error}")))?,
            ),
            None => None,
        },
        Err(error) => return Err(Error(format!("invalid source map reference: {error}"))),
    };
    if let Some(url) = &url
        && (!matches!(url.scheme(), "http" | "https") || url.host_str().is_none())
    {
        return Err(Error("external source map URL must use HTTP(S)".into()));
    }
    Ok(url)
}
