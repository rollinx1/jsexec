use super::{Report, SectionReference, SourceEntry, SourceMap, resolve_map_url};
use serde_json::Value;
use url::Url;

pub(super) fn parse(json: &str, map: &mut SourceMap, report: &mut Report) {
    let value = match serde_json::from_str::<Value>(json.trim_start_matches('\u{feff}')) {
        Ok(value) => value,
        Err(error) => {
            report.diagnostic(&map.input, format!("invalid source map JSON: {error}"));
            return;
        }
    };
    map.generated_file = value.get("file").and_then(Value::as_str).map(str::to_owned);
    flatten(&value, &[], map, report);
}

fn flatten(value: &Value, section: &[usize], map: &mut SourceMap, report: &mut Report) {
    let input = map.input.clone();
    let issue = |report: &mut Report, message: String| {
        report.diagnostic(&input, format!("section {section:?}: {message}"))
    };
    if section.len() > 32 {
        issue(report, "source map section nesting exceeds 32".into());
        return;
    }
    let Some(object) = value.as_object() else {
        issue(report, "source map must be an object".into());
        return;
    };
    if object.get("version").and_then(Value::as_u64) != Some(3) {
        issue(report, "source map version must be 3".into());
        return;
    }
    if let Some(sections) = object.get("sections") {
        let Some(sections) = sections.as_array() else {
            issue(report, "sections must be an array".into());
            return;
        };
        if object.contains_key("sources") {
            issue(
                report,
                "index map contains both sections and sources".into(),
            );
        }
        for (index, child) in sections.iter().enumerate() {
            let mut child_section = section.to_vec();
            child_section.push(index);
            if let Some(embedded) = child.get("map") {
                // Section maps are complete maps; sourceRoot is not inherited (ECMA-426).
                flatten(embedded, &child_section, map, report);
            } else if let Some(reference) = child.get("url").and_then(Value::as_str) {
                // Keep legacy URL sections visible, without downloading them.
                let base = map.url.as_deref().and_then(|url| Url::parse(url).ok());
                let url = resolve_map_url(reference, base.as_ref());
                if let Err(error) = &url {
                    issue(report, error.to_string());
                }
                map.external_sections.push(SectionReference {
                    section: child_section,
                    reference: reference.into(),
                    url: url.ok().flatten().map(|url| url.to_string()),
                });
            } else {
                issue(report, format!("section {index} is missing a map or URL"));
            }
        }
        return;
    }
    let Some(sources) = object.get("sources").and_then(Value::as_array) else {
        issue(report, "missing sources array".into());
        return;
    };
    let root = match object.get("sourceRoot") {
        Some(Value::String(root)) => Some(root.as_str()),
        None | Some(Value::Null) => None,
        Some(_) => {
            issue(report, "sourceRoot must be a string or null".into());
            None
        }
    };
    let contents = match object.get("sourcesContent") {
        Some(Value::Array(contents)) => {
            if contents.len() != sources.len() {
                issue(
                    report,
                    format!(
                        "sources/content length mismatch: {} vs {}",
                        sources.len(),
                        contents.len()
                    ),
                );
            }
            Some(contents)
        }
        None | Some(Value::Null) => None,
        Some(_) => {
            issue(report, "sourcesContent must be an array".into());
            None
        }
    };
    let ignored = object
        .get("ignoreList")
        .or_else(|| object.get("x_google_ignoreList"))
        .and_then(Value::as_array);
    for (index, source) in sources.iter().enumerate() {
        let path = match source {
            Value::String(path) => Some(path.clone()),
            Value::Null => None,
            _ => {
                issue(report, format!("source {index} is not a string or null"));
                None
            }
        };
        let content = match contents.and_then(|contents| contents.get(index)) {
            Some(Value::String(content)) => Some(content.clone()),
            None | Some(Value::Null) => None,
            Some(_) => {
                issue(
                    report,
                    format!("source {index} content is not a string or null"),
                );
                None
            }
        };
        let resolved_url = path
            .as_deref()
            .and_then(|path| resolve_source(path, root, map.url.as_deref()));
        map.sources.push(SourceEntry {
            index,
            section: section.to_vec(),
            path,
            source_root: root.map(str::to_owned),
            resolved_url,
            content,
            ignored: ignored.is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_u64() == Some(index as u64))
            }),
            extracted_path: None,
        });
    }
}

fn resolve_source(path: &str, root: Option<&str>, map_url: Option<&str>) -> Option<String> {
    let reference = match root.filter(|root| !root.is_empty()) {
        Some(root) => format!("{root}{}{path}", if root.ends_with('/') { "" } else { "/" }),
        None => path.to_string(),
    };
    let url = Url::parse(&reference)
        .ok()
        .or_else(|| Url::parse(map_url?).ok()?.join(&reference).ok())?;
    // Preserve virtual webpack/vite/file URLs as evidence; never invent a web origin.
    Some(url.to_string())
}
