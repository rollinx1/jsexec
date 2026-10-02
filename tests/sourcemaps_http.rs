use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAP: &str = r#"{"version":3,"sources":["src/app.ts"],"sourcesContent":["export const answer = 42;"],"mappings":""}"#;

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Response {
    fn new(status: u16, headers: &[(&str, &str)], body: &str) -> Self {
        Self {
            status,
            headers: headers
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop_thread = stop.clone();
        let requests_thread = requests.clone();
        let thread = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let request = read_request(&mut stream);
                        requests_thread.lock().unwrap().push(request.clone());
                        let response = handler(&request);
                        let mut bytes = format!(
                            "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n",
                            response.status,
                            response.body.len()
                        );
                        for (name, value) in response.headers {
                            bytes.push_str(&format!("{name}: {value}\r\n"));
                        }
                        bytes.push_str("\r\n");
                        let _ = stream.write_all(bytes.as_bytes());
                        let _ = stream.write_all(&response.body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 32_768 {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}
fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jsexec"))
        .arg("sourcemaps")
        .args(args)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .output()
        .unwrap()
}
fn report(args: &[&str]) -> Value {
    let output = run(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn authenticated_script_headers_override_comments_and_follow_relative_maps() {
    let server = Server::new(|request| {
        assert!(request.contains("authorization: Bearer test-token\r\n"));
        assert!(request.contains("cookie: session=test\r\n"));
        assert_eq!(request.matches("x-tag:").count(), 2);
        if request.starts_with("GET /assets/app.js?") {
            Response::new(
                200,
                &[("SourceMap", "maps/app.map"), ("X-SourceMap", "wrong.map")],
                "const a = 1;\n//# sourceMappingURL=also-wrong.map",
            )
        } else {
            assert!(request.starts_with("GET /assets/maps/app.map "));
            Response::new(200, &[], MAP)
        }
    });
    let input = format!("{}/assets/app.js?version=1", server.url);
    let result = report(&[
        &input,
        "-H",
        "Authorization: Bearer test-token",
        "--header",
        "Cookie: session=test",
        "-H",
        "X-Tag: one",
        "-H",
        "X-Tag: two",
    ]);
    assert_eq!(result["diagnostics"], json!([]));
    assert_eq!(result["maps"][0]["kind"], "external");
    assert_eq!(result["maps"][0]["reference"], "maps/app.map");
    assert_eq!(
        result["maps"][0]["sources"][0]["content"],
        "export const answer = 42;"
    );
    assert_eq!(
        result["maps"][0]["sources"][0]["resolved_url"],
        format!("{}/assets/maps/src/app.ts", server.url)
    );
    assert!(result["maps"][0].get("location").is_none());
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn direct_map_urls_and_inline_maps_need_one_request() {
    let server = Server::new(|request| {
        if request.starts_with("GET /inline.js ") {
            Response::new(
                200,
                &[(
                    "X-SourceMap",
                    "data:application/json,%7B%22version%22:3,%22sources%22:[],%22mappings%22:%22%22%7D",
                )],
                "const a = 1;",
            )
        } else {
            Response::new(200, &[], MAP)
        }
    });
    let result = report(&[&format!("{}/app.js.map?v=2", server.url)]);
    assert_eq!(result["maps"][0]["kind"], "file");
    assert_eq!(
        result["maps"][0]["sources"][0]["content"],
        "export const answer = 42;"
    );
    let result = report(&[&format!("{}/inline.js", server.url)]);
    assert_eq!(result["maps"][0]["kind"], "inline");
    assert_eq!(result["diagnostics"], json!([]));
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn redirects_resolve_annotations_against_final_script_url_and_final_map_url() {
    let server = Server::new(|request| {
        if request.starts_with("GET /start ") {
            Response::new(302, &[("Location", "/assets/app.js")], "")
        } else if request.starts_with("GET /assets/app.js ") {
            Response::new(200, &[], "//# sourceMappingURL=app.map")
        } else if request.starts_with("GET /assets/app.map ") {
            Response::new(307, &[("Location", "/maps/final.map")], "")
        } else {
            assert!(request.starts_with("GET /maps/final.map "));
            Response::new(200, &[], MAP)
        }
    });
    let result = report(&[&format!("{}/start", server.url)]);
    assert_eq!(
        result["maps"][0]["input"],
        format!("{}/assets/app.js", server.url)
    );
    assert_eq!(
        result["maps"][0]["url"],
        format!("{}/maps/final.map", server.url)
    );
    assert_eq!(
        result["maps"][0]["sources"][0]["resolved_url"],
        format!("{}/maps/src/app.ts", server.url)
    );
}

#[test]
fn custom_headers_are_removed_on_cross_origin_links_and_redirects() {
    let destination = Server::new(|request| {
        assert!(!request.contains("test-secret"));
        Response::new(200, &[], MAP)
    });
    let target = format!("{}/app.map", destination.url);
    let server = Server::new(move |request| {
        assert!(request.contains("test-secret"));
        if request.starts_with("GET /redirect ") {
            Response::new(302, &[("Location", &target)], "")
        } else {
            Response::new(200, &[("SourceMap", &target)], "const a = 1;")
        }
    });
    for path in ["/app.js", "/redirect"] {
        let result = report(&[
            &format!("{}{path}", server.url),
            "-H",
            "Authorization: test-secret",
            "-H",
            "X-Api-Key: test-secret",
        ]);
        assert_eq!(result["diagnostics"], json!([]));
        assert_eq!(
            result["maps"][0]["sources"][0]["content"],
            "export const answer = 42;"
        );
    }
    assert_eq!(destination.requests().len(), 2);
}

#[test]
fn list_and_no_fetch_skip_map_downloads_and_local_files_require_fetch() {
    let server = Server::new(|request| {
        if request.starts_with("GET /app.js ") {
            Response::new(200, &[], "//# sourceMappingURL=app.map")
        } else {
            assert!(request.starts_with("GET /app.map "));
            Response::new(200, &[], MAP)
        }
    });
    let script = format!("{}/app.js", server.url);
    let output = run(&[&script, "--list"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}/app.map\n", server.url)
    );
    let result = report(&[&script, "--no-fetch"]);
    assert_eq!(result["maps"][0]["sources"], json!([]));
    assert_eq!(server.requests().len(), 2);
    let input = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/runtime.js");
    // Local input and even its HTTP base URL perform no requests by default.
    report(&[input, "--base-url", &script]);
    assert_eq!(server.requests().len(), 2);
    let dir = std::env::temp_dir().join(format!("jsexec-http-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("app.js");
    std::fs::write(&path, "//# sourceMappingURL=app.map").unwrap();
    let recovered = dir.join("recovered");
    let result = report(&[
        path.to_str().unwrap(),
        "--base-url",
        &script,
        "--fetch",
        "--sources-dir",
        recovered.to_str().unwrap(),
    ]);
    assert_eq!(
        result["maps"][0]["sources"][0]["content"],
        "export const answer = 42;"
    );
    assert_eq!(
        std::fs::read_to_string(recovered.join("map-0001/source-0001/src/app.ts")).unwrap(),
        "export const answer = 42;"
    );
    assert_eq!(server.requests().len(), 3);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failures_do_not_write_reports_or_sources_and_headers_are_not_echoed() {
    let server = Server::new(|request| {
        if request.starts_with("GET /missing ") {
            Response::new(404, &[], "missing")
        } else if request.starts_with("GET /loop ") {
            Response::new(302, &[("Location", "/loop")], "")
        } else if request.starts_with("GET /invalid.map ") {
            Response::new(200, &[], "{broken")
        } else if request.starts_with("GET /binary ") {
            let mut r = Response::new(200, &[], "");
            r.body = vec![255];
            r
        } else {
            Response::new(200, &[], MAP)
        }
    });
    for (path, args, expected) in [
        ("/missing", vec![], "404"),
        ("/loop", vec![], "redirect limit"),
        ("/app.map", vec!["--max-bytes", "10"], "exceeds --max-bytes"),
        ("/binary", vec![], "not UTF-8"),
        ("/invalid.map", vec!["--strict"], "source diagnostic"),
    ] {
        let url = format!("{}{path}", server.url);
        let mut command_args = vec![url.as_str()];
        command_args.extend(args);
        let output = run(&command_args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let count = server.requests().len();
    for header in [
        "secret-without-colon",
        "Bad Name: secret-value",
        "Authorization: secret\r\nInjected: bad",
    ] {
        let output = run(&[&format!("{}/app.map", server.url), "-H", header]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    }
    assert_eq!(server.requests().len(), count);
    let dir = std::env::temp_dir().join(format!("jsexec-http-strict-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let output_path = dir.join("report.json");
    let sources = dir.join("sources");
    std::fs::write(&output_path, "existing").unwrap();
    let output = run(&[
        &format!("{}/invalid.map", server.url),
        "--strict",
        "-o",
        output_path.to_str().unwrap(),
        "--sources-dir",
        sources.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(output_path).unwrap(), "existing");
    assert!(!sources.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn timeout_and_url_validation_fail_cleanly() {
    let server = Server::new(|_| {
        thread::sleep(Duration::from_millis(1300));
        Response::new(200, &[], MAP)
    });
    let output = run(&[&format!("{}/app.map", server.url), "--timeout", "1"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let output = run(&["https://user:secret@example.com/app.map"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    let output = run(&["https://example.com/app.js", "--fetch", "--no-fetch"]);
    assert!(!output.status.success());
}

#[test]
fn compressed_responses_are_decoded_and_limited_after_decompression() {
    let server = Server::new(|_| Response {
        status: 200,
        headers: vec![("Content-Encoding".into(), "gzip".into())],
        body: vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 42, 75, 45, 42, 206, 204, 207, 83, 178, 50,
            214, 81, 42, 206, 47, 45, 74, 78, 45, 86, 178, 138, 86, 74, 44, 40, 208, 203, 42, 86,
            138, 133, 11, 58, 231, 231, 149, 164, 230, 149, 128, 228, 42, 70, 56, 0, 133, 74, 46,
            48, 128, 50, 243, 210, 129, 97, 165, 164, 84, 11, 0, 204, 19, 210, 146, 70, 1, 0, 0,
        ],
    });
    let url = format!("{}/app.map", server.url);
    let result = report(&[&url]);
    assert_eq!(result["diagnostics"], json!([]));
    assert_eq!(
        result["maps"][0]["sources"][0]["content"]
            .as_str()
            .unwrap()
            .len(),
        256
    );
    let output = run(&[&url, "--max-bytes", "200"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exceeds --max-bytes"));
}

#[test]
fn stdin_can_fetch_absolute_map_links_with_headers_without_a_base_url() {
    let server = Server::new(|request| {
        assert!(request.contains("authorization: test-token"));
        Response::new(200, &[], MAP)
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_jsexec"))
        .args([
            "sourcemaps",
            "-",
            "--fetch",
            "-H",
            "Authorization: test-token",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("NO_PROXY", "*")
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        "//# sourceMappingURL={}/app.map",
        server.url
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["maps"][0]["input"], "<stdin>");
    assert_eq!(
        result["maps"][0]["sources"][0]["content"],
        "export const answer = 42;"
    );
}
