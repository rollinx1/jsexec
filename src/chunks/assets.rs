use url::Url;

pub fn is_javascript_asset(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Ok(base) = Url::parse("https://jsexec.invalid/") else {
        return false;
    };
    let Ok(url) = base.join(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return false;
    }
    let Some(filename) = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
    else {
        return false;
    };
    let name = filename.to_ascii_lowercase();
    [".js", ".mjs", ".cjs"].iter().any(|extension| {
        name.strip_suffix(extension)
            .is_some_and(|stem| !stem.is_empty())
    })
}

// Literal scanning is deliberately narrower than imports and manifest evidence.
pub fn is_direct_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    is_javascript_asset(value)
        && (lower.starts_with("http://")
            || lower.starts_with("https://")
            || value.starts_with('/')
            || value.starts_with("./")
            || value.starts_with("../")
            || value.starts_with("assets/"))
}
