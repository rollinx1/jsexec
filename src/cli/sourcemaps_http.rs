use super::{SourceMapArgs, read_sources};
use jsexec::source::Source;
use jsexec::sourcemaps::{self, InputKind, MapKind, Options, Report};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use url::Url;

pub(super) fn is_url(input: &str) -> bool {
    input
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
        || input
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
}

fn validate_url(url: &Url) -> Result<(), Box<dyn Error>> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("retrieval requires an absolute HTTP(S) URL".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL credentials are unsupported; use --header instead".into());
    }
    Ok(())
}

fn parse_headers(values: &[String]) -> Result<HeaderMap, Box<dyn Error>> {
    let mut headers = HeaderMap::new();
    for (index, value) in values.iter().enumerate() {
        let (name, value) = value
            .split_once(':')
            .ok_or_else(|| format!("header {} must use NAME: VALUE syntax", index + 1))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| format!("invalid name in header {}", index + 1))?;
        let mut value = HeaderValue::from_str(value.trim())
            .map_err(|_| format!("invalid value in header {}", index + 1))?;
        value.set_sensitive(true);
        headers.append(name, value);
    }
    Ok(headers)
}

struct Download {
    source: Source,
    url: Url,
    map_header: Option<String>,
}

struct Http {
    client: Client,
    headers: HeaderMap,
    timeout: Duration,
    max_bytes: u64,
}

impl Http {
    fn get(&self, mut url: Url, header_origin: Option<&Url>) -> Result<Download, Box<dyn Error>> {
        let start = Instant::now();
        let mut send_headers = header_origin.is_some_and(|origin| origin.origin() == url.origin());
        for redirects in 0..=10 {
            validate_url(&url)?;
            let remaining = self
                .timeout
                .checked_sub(start.elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or("HTTP request timed out")?;
            let mut request = self.client.get(url.clone()).timeout(remaining);
            if send_headers {
                request = request.headers(self.headers.clone());
            }
            let response = request.send().map_err(|error| error.without_url())?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                if redirects == 10 {
                    return Err("HTTP redirect limit exceeded (10)".into());
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or("HTTP redirect is missing Location")?
                    .to_str()
                    .map_err(|_| "invalid HTTP redirect Location")?;
                let next = url
                    .join(location)
                    .map_err(|_| "invalid HTTP redirect URL")?;
                validate_url(&next)?;
                if url.scheme() == "https" && next.scheme() == "http" {
                    return Err("refusing an HTTPS-to-HTTP redirect".into());
                }
                // Strip every custom header permanently after an origin change.
                send_headers &= url.origin() == next.origin();
                url = next;
                continue;
            }
            if !response.status().is_success() {
                return Err(
                    format!("HTTP request failed with status {}", response.status()).into(),
                );
            }
            let map_header = response
                .headers()
                .get("sourcemap")
                .or_else(|| response.headers().get("x-sourcemap"))
                .map(|value| value.to_str().map(str::to_owned))
                .transpose()
                .map_err(|_| "invalid SourceMap response header")?;
            let mut bytes = Vec::new();
            response
                .take(self.max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > self.max_bytes {
                return Err(
                    format!("HTTP response exceeds --max-bytes ({})", self.max_bytes).into(),
                );
            }
            let code = String::from_utf8(bytes).map_err(|_| "HTTP response is not UTF-8")?;
            return Ok(Download {
                source: Source {
                    name: url.to_string(),
                    code,
                },
                url,
                map_header,
            });
        }
        unreachable!()
    }
}

pub(super) fn analyze_inputs(
    args: &SourceMapArgs,
    options: &Options,
) -> Result<Report, Box<dyn Error>> {
    if args
        .files
        .iter()
        .filter(|input| input.as_str() == "-")
        .count()
        > 1
    {
        return Err("stdin ('-') can only be supplied once".into());
    }
    // Reject malformed arguments before reading stdin or making any requests.
    let headers = parse_headers(&args.headers)?;
    for input in &args.files {
        if is_url(input) {
            validate_url(&Url::parse(input)?)?;
        }
    }
    if let Some(base) = &options.base_url {
        validate_url(base)?;
    }
    let http = Http {
        client: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("jsexec/", env!("CARGO_PKG_VERSION")))
            .build()?,
        headers,
        timeout: Duration::from_secs(args.timeout),
        max_bytes: args.max_bytes,
    };
    let mut report = Report::default();
    for input in &args.files {
        let remote = is_url(input);
        let (source, base, header, origin) = if remote {
            let url = Url::parse(input)?;
            let download = http.get(url.clone(), Some(&url))?;
            (
                download.source,
                Some(download.url),
                download.map_header,
                Some(url),
            )
        } else {
            let mut sources = read_sources(vec![PathBuf::from(input)])?;
            (
                sources.remove(0),
                options.base_url.clone(),
                None,
                options.base_url.clone(),
            )
        };
        let mut input_report = sourcemaps::analyze_response(
            &source,
            &Options {
                base_url: base,
                input_kind: options.input_kind,
            },
            header.as_deref(),
        )?;
        if !args.no_fetch && !args.list && (remote || args.fetch) {
            for map in &mut input_report.maps {
                if map.kind != MapKind::External {
                    continue;
                }
                let map_url = map
                    .url
                    .as_deref()
                    .ok_or("cannot fetch a relative map reference; supply --base-url")?;
                let url = Url::parse(map_url)?;
                if origin
                    .as_ref()
                    .is_some_and(|origin| origin.scheme() == "https" && url.scheme() == "http")
                {
                    return Err("refusing an HTTPS-to-HTTP source map link".into());
                }
                let download = http.get(url.clone(), origin.as_ref().or(Some(&url)))?;
                let mut recovered = sourcemaps::analyze_response(
                    &download.source,
                    &Options {
                        base_url: Some(download.url),
                        input_kind: InputKind::Map,
                    },
                    None,
                )?;
                let loaded = recovered.maps.remove(0);
                map.url = loaded.url;
                map.generated_file = loaded.generated_file;
                map.sources = loaded.sources;
                map.external_sections = loaded.external_sections;
                input_report.diagnostics.append(&mut recovered.diagnostics);
            }
        }
        report.maps.append(&mut input_report.maps);
        report.diagnostics.append(&mut input_report.diagnostics);
    }
    Ok(report)
}
