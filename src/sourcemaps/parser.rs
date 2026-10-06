use super::{Report, SectionReference, SourceEntry, SourceMap, resolve_map_url};
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value, value::RawValue};
use std::collections::BTreeMap;
use std::fmt;
use url::Url;

// Keep only borrowed slices for fields recovery uses. Large `mappings`, `names`,
// and vendor extensions are validated and skipped without allocating their values.
struct RawFields<'a>(BTreeMap<String, &'a RawValue>);
impl<'de> Deserialize<'de> for RawFields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = RawFields<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a source map object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if matches!(
                        key.as_str(),
                        "version"
                            | "file"
                            | "sections"
                            | "sources"
                            | "sourceRoot"
                            | "sourcesContent"
                            | "ignoreList"
                            | "x_google_ignoreList"
                            | "map"
                            | "url"
                    ) {
                        fields.insert(key, map.next_value::<&'de RawValue>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(RawFields(fields))
            }
        }
        deserializer.deserialize_map(FieldsVisitor)
    }
}

pub(super) fn parse(json: &str, map: &mut SourceMap, report: &mut Report) {
    let raw = match serde_json::from_str::<&RawValue>(json.trim_start_matches('\u{feff}')) {
        Ok(value) => value,
        Err(error) => {
            report.diagnostic(&map.input, format!("invalid source map JSON: {error}"));
            return;
        }
    };
    flatten(raw, &[], map, report);
}

fn flatten(raw: &RawValue, section: &[usize], map: &mut SourceMap, report: &mut Report) {
    let input = map.input.clone();
    let issue = |report: &mut Report, message: String| {
        report.diagnostic(&input, format!("section {section:?}: {message}"))
    };
    if section.len() > 32 {
        issue(report, "source map section nesting exceeds 32".into());
        return;
    }
    let Ok(RawFields(object)) = serde_json::from_str::<RawFields<'_>>(raw.get()) else {
        issue(report, "source map must be an object".into());
        return;
    };
    if section.is_empty() {
        map.generated_file = object
            .get("file")
            .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok());
    }
    if object
        .get("version")
        .and_then(|value| value.get().trim().parse::<u64>().ok())
        != Some(3)
    {
        issue(report, "source map version must be 3".into());
        return;
    }
    if let Some(sections) = object.get("sections") {
        let Ok(sections) = serde_json::from_str::<Vec<&RawValue>>(sections.get()) else {
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
            let fields = serde_json::from_str::<RawFields<'_>>(child.get()).ok();
            if let Some(embedded) = fields.as_ref().and_then(|fields| fields.0.get("map")) {
                flatten(embedded, &child_section, map, report);
            } else if let Some(reference) = fields
                .as_ref()
                .and_then(|fields| fields.0.get("url"))
                .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
            {
                let base = map.url.as_deref().and_then(|url| Url::parse(url).ok());
                let url = resolve_map_url(&reference, base.as_ref());
                if let Err(error) = &url {
                    issue(report, error.to_string());
                }
                map.external_sections.push(SectionReference {
                    section: child_section,
                    reference,
                    url: url.ok().flatten().map(|url| url.to_string()),
                });
            } else {
                issue(report, format!("section {index} is missing a map or URL"));
            }
        }
        return;
    }
    let Some(sources) = object
        .get("sources")
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value.get()).ok())
    else {
        issue(report, "missing sources array".into());
        return;
    };
    let decode = |name: &str| {
        object
            .get(name)
            .map(|value| serde_json::from_str::<Value>(value.get()))
            .transpose()
    };
    let root_value = match decode("sourceRoot") {
        Ok(value) => value,
        Err(error) => {
            issue(report, format!("invalid sourceRoot JSON: {error}"));
            return;
        }
    };
    let root = match root_value {
        Some(Value::String(root)) => Some(root),
        None | Some(Value::Null) => None,
        Some(_) => {
            issue(report, "sourceRoot must be a string or null".into());
            None
        }
    };
    let content_value = match decode("sourcesContent") {
        Ok(value) => value,
        Err(error) => {
            issue(report, format!("invalid sourcesContent JSON: {error}"));
            return;
        }
    };
    let contents = match content_value {
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
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value.get()).ok());
    let mut contents = contents.unwrap_or_default().into_iter();
    for (index, source) in sources.into_iter().enumerate() {
        let path = match source {
            Value::String(path) => Some(path),
            Value::Null => None,
            _ => {
                issue(report, format!("source {index} is not a string or null"));
                None
            }
        };
        let content = match contents.next() {
            Some(Value::String(content)) => Some(content),
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
            .and_then(|path| resolve_source(path, root.as_deref(), map.url.as_deref()));
        map.sources.push(SourceEntry {
            index,
            section: section.to_vec(),
            path,
            source_root: root.clone(),
            resolved_url,
            content,
            ignored: ignored.as_ref().is_some_and(|values| {
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
