use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jsexec"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "jsexec-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, content: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        path.to_str().unwrap().into()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn stdin_produces_json_with_evidence() {
    let output = run(
        &["chunks", "-", "--extractor", "imports"],
        "import('./login.js');",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["chunks"][0]["value"], "./login.js");
    assert_eq!(report["chunks"][0]["evidence"][0]["file"], "<stdin>");
    assert_eq!(report["chunks"][0]["evidence"][0]["location"]["line"], 1);
    assert_eq!(report["diagnostics"], serde_json::json!([]));
}

#[test]
fn multiple_files_deduplicate_and_list_resolved_urls() {
    let workspace = Workspace::new();
    let first = workspace.file("a.js", "import('./login.js');");
    let second = workspace.file("b.js", "import('./login.js');import('./other.js');");
    let output = run(
        &[
            "chunks",
            &first,
            &second,
            "--extractor",
            "imports,webpack",
            "--base-url",
            "https://example.com/assets/main.js",
            "--list",
        ],
        "",
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "https://example.com/assets/login.js\nhttps://example.com/assets/other.js\n"
    );
}

#[test]
fn output_file_receives_results_and_input_is_protected() {
    let workspace = Workspace::new();
    let input = workspace.file("a.js", "import('./login.js');");
    let output_path = workspace.0.join("chunks.json");
    let output = run(
        &["extract", &input, "--output", output_path.to_str().unwrap()],
        "",
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&output_path).unwrap()).is_ok()
    );
    let output = run(&["chunks", &input, "--output", &input], "");
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "import('./login.js');"
    );
}

#[test]
fn strict_failures_preserve_existing_output() {
    let workspace = Workspace::new();
    let output_path = workspace.file("output.json", "original output");
    let output = run(
        &["chunks", "-", "--strict", "--output", &output_path],
        "return unexpected;",
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(output_path).unwrap(),
        "original output"
    );
}

#[test]
fn input_type_can_override_stdin_detection() {
    let output = run(
        &["chunks", "-", "--input-type", "json", "--list"],
        r#"{"assets":["app.js","style.css"]}"#,
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"app.js\n");
}

#[test]
fn invalid_options_missing_files_and_duplicate_stdin_fail_cleanly() {
    for args in [
        vec!["chunks", "-", "--extractor", "unknown"],
        vec!["chunks", "-", "--base-url", "file:///tmp/"],
        vec!["chunks", "-", "--base-url", "relative"],
        vec!["chunks", "-", "-"],
        vec!["chunks", "/missing/jsexec-test-file.js"],
        vec!["chunks"],
    ] {
        let output = run(&args, "");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn extractor_listing_matches_the_library_registry() {
    let output = run(&["extractors"], "");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "imports\nreferences\nvite\nwebpack\nmanifest\nhtml\n"
    );
}

#[test]
fn sourcemap_urls_are_listed_and_deduplicated_across_inputs() {
    let workspace = Workspace::new();
    let a = workspace.file("a.js", "//# sourceMappingURL=maps/a.map");
    let b = workspace.file("b.js", "/*# sourceMappingURL=maps/a.map */");
    let output = run(
        &[
            "sourcemaps",
            &a,
            &b,
            "--base-url",
            "https://example.com/assets/main.js",
            "--list",
        ],
        "",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"https://example.com/assets/maps/a.map\n");
}

#[test]
fn source_extraction_preserves_duplicates_empty_files_and_contains_unsafe_paths() {
    let workspace = Workspace::new();
    let directory = workspace.0.join("recovered");
    let json = serde_json::json!({"version":3,"sources":["../../escape.ts","/etc/absolute.js","webpack://project/./src/a.ts","same.ts","same.ts","missing.ts",null,"C:\\outside\\file.js"],"sourcesContent":["first","absolute","virtual","one","",null,"anonymous","windows"]}).to_string();
    let output = run(
        &[
            "sourcemaps",
            "-",
            "--input-type",
            "map",
            "--sources-dir",
            directory.to_str().unwrap(),
        ],
        &json,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for (index, source) in report["maps"][0]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        if source["content"].is_null() {
            assert!(source.get("extracted_path").is_none());
            continue;
        }
        let relative = std::path::Path::new(source["extracted_path"].as_str().unwrap());
        assert!(!relative.is_absolute());
        assert!(
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        );
        assert_eq!(
            std::fs::read_to_string(directory.join(relative)).unwrap(),
            source["content"].as_str().unwrap(),
            "{index}"
        );
    }
    assert_ne!(
        report["maps"][0]["sources"][3]["extracted_path"],
        report["maps"][0]["sources"][4]["extracted_path"]
    );
    assert!(!workspace.0.join("escape.ts").exists());
}

#[test]
fn sourcemap_strict_errors_create_no_extraction_directory_or_output() {
    let workspace = Workspace::new();
    let directory = workspace.0.join("recovered");
    let output_path = workspace.file("report.json", "original");
    let output = run(
        &[
            "sourcemaps",
            "-",
            "--input-type",
            "map",
            "--sources-dir",
            directory.to_str().unwrap(),
            "--strict",
            "--output",
            &output_path,
        ],
        r#"{"version":3,"sources":["a.ts","b.ts"],"sourcesContent":["one"]}"#,
    );
    assert!(!output.status.success());
    assert!(!directory.exists());
    assert_eq!(std::fs::read_to_string(output_path).unwrap(), "original");
}

#[test]
fn sourcemap_extraction_rejects_existing_directories_and_empty_recovery() {
    let workspace = Workspace::new();
    let output = run(
        &[
            "sourcemaps",
            "-",
            "--sources-dir",
            workspace.0.to_str().unwrap(),
        ],
        "",
    );
    assert!(!output.status.success());
    let directory = workspace.0.join("new");
    let output = run(
        &[
            "sourcemaps",
            "-",
            "--sources-dir",
            directory.to_str().unwrap(),
        ],
        "//# sourceMappingURL=external.map",
    );
    assert!(!output.status.success());
    assert!(!directory.exists());
}

#[test]
fn inline_sourcemap_cli_recovers_original_content() {
    use base64::Engine;
    let map = r#"{"version":3,"sources":["a.ts"],"sourcesContent":["const a: number = 1;"]}"#;
    let input = format!(
        "const a = 1;\n//# sourceMappingURL=data:application/json;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(map)
    );
    let output = run(&["sourcemap", "-"], &input);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["maps"][0]["kind"], "inline");
    assert_eq!(
        report["maps"][0]["sources"][0]["content"],
        "const a: number = 1;"
    );
}

#[test]
fn sourcemap_reports_cannot_overwrite_recovered_source_files() {
    let workspace = Workspace::new();
    let directory = workspace.0.join("recovered");
    let destination = directory.join("map-0001/source-0001/a.ts");
    let output = run(
        &[
            "sourcemaps",
            "-",
            "--input-type",
            "json",
            "--sources-dir",
            directory.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
        ],
        r#"{"version":3,"sources":["a.ts"],"sourcesContent":["original source"]}"#,
    );
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "original source"
    );
}

#[test]
fn ast_discovery_query_projection_and_node_navigation_work_together() {
    let workspace = Workspace::new();
    let input = workspace.file(
        "app.ts",
        "async function load(id: number) { return fetch(`/api/${id}`); }\nload(3);",
    );
    let output = run(&["ast", &input], "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ast: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(ast[0]["node_count"].as_u64().unwrap() > 10);
    assert!(
        ast[0]["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["kind"] == "TSNumberKeyword")
    );
    assert_eq!(ast[0]["partial"], false);
    let output = run(
        &[
            "query",
            "CallExpression[callee.name='fetch']",
            &input,
            "--fields",
            "callee.name,arguments.*.type",
            "--limit",
            "1",
        ],
        "",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report[0]["match_count"], 1);
    assert_eq!(report[0]["nodes"][0]["fields"]["callee.name"], "fetch");
    assert_eq!(
        report[0]["nodes"][0]["fields"]["arguments.*.type"],
        serde_json::json!(["TemplateLiteral"])
    );
    let hash = report[0]["hash"].as_str().unwrap();
    assert_eq!(hash, ast[0]["hash"]);
    let id = report[0]["nodes"][0]["id"].as_str().unwrap();
    let output = run(
        &[
            "node",
            &input,
            id,
            "--relation",
            "context",
            "--expect-hash",
            hash,
        ],
        "",
    );
    assert!(output.status.success());
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context[0]["nodes"][0]["kind"], "FunctionDeclaration");
    std::fs::write(&input, "fetch('/changed');").unwrap();
    let output = run(&["node", &input, id, "--expect-hash", hash], "");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("source hash mismatch"));
    assert!(output.stdout.is_empty());
}

#[test]
fn query_paginates_each_input_and_reads_stdin() {
    let workspace = Workspace::new();
    let input = workspace.file("a.js", "a(); b(); c();");
    let output = run(
        &[
            "query",
            "CallExpression",
            &input,
            "-",
            "--limit",
            "1",
            "--offset",
            "1",
        ],
        "x(); y();",
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report[0]["match_count"], 3);
    assert_eq!(report[0]["nodes"][0]["code"], "b()");
    assert_eq!(report[0]["next_offset"], 2);
    assert_eq!(report[1]["file"], "<stdin>");
    assert_eq!(report[1]["nodes"][0]["code"], "y()");
    assert_eq!(report[1]["next_offset"], serde_json::Value::Null);
    let output = run(&["query", "*", &input, "--limit", "0"], "");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report[0]["match_count"].as_u64().unwrap() > 3);
    assert_eq!(report[0]["nodes"], serde_json::json!([]));
}

#[test]
fn query_failures_do_not_write_and_partial_results_are_marked() {
    let workspace = Workspace::new();
    let input = workspace.file("bad.js", "const = ;");
    let output_path = workspace.file("report.json", "original");
    for args in [
        vec!["query", "[", &input, "-o", &output_path],
        vec!["query", "*", &input, "--strict", "-o", &output_path],
        vec!["ast", &input, "--strict", "-o", &output_path],
        vec!["node", &input, "n999999999", "-o", &output_path],
        vec!["query", "*", &input, "-o", &input],
    ] {
        let output = run(&args, "");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read_to_string(&output_path).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "const = ;");
    }
    let output = run(&["query", "*", &input], "");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report[0]["partial"], true);
    assert!(!report[0]["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn formatting_defaults_to_source_on_stdout_and_preserves_input() {
    let workspace = Workspace::new();
    let original = "function load(id){return fetch('/api/'+id)}";
    let input = workspace.file("app.js", original);
    let output = run(&["format", &input], "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("\n  return fetch(\"/api/\" + id);\n")
    );
    assert_eq!(std::fs::read_to_string(input).unwrap(), original);
}

#[test]
fn formatting_stdin_supports_typescript_html_and_options() {
    let output = run(
        &[
            "format",
            "-",
            "--input-type",
            "ts",
            "--single-quote",
            "--indent-width",
            "4",
        ],
        "function f(){const v: string=\"hi\";return v}",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\n    const v: string = 'hi';"));
    let output = run(
        &["format", "-", "--input-type", "html"],
        "<main><p>one</p><p>two</p></main>",
    );
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("<main>\n      <p>one</p>\n      <p>two</p>")
    );
    let output = run(&["format", "-"], "const a=1;");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"const a = 1;\n");
}

#[test]
fn format_output_write_and_check_modes_are_explicit() {
    let workspace = Workspace::new();
    let input = workspace.file("app.ts", "const value: number=1;");
    let output_path = workspace.0.join("formatted.ts");
    let output = run(
        &["format", &input, "--output", output_path.to_str().unwrap()],
        "",
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(&output_path).unwrap(),
        "const value: number = 1;\n"
    );
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "const value: number=1;"
    );
    let output = run(&["format", &input, "--check"], "");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs formatting"));
    let output = run(&["format", &input, "--write"], "");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "const value: number = 1;\n"
    );
    let output = run(&["format", &input, "--check"], "");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn formatting_parse_failures_preserve_input_and_existing_output() {
    let workspace = Workspace::new();
    let input = workspace.file("invalid.js", "const = ;");
    let destination = workspace.file("out.js", "original output");
    for args in [
        vec!["format", &input, "--write"],
        vec!["format", &input, "--output", &destination],
    ] {
        let output = run(&args, "");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "const = ;");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "original output"
        );
    }
}

#[test]
fn formatting_rejects_conflicting_options_and_accidental_input_overwrite() {
    let workspace = Workspace::new();
    let input = workspace.file("app.js", "const x=1;");
    let destination = workspace.file("out.js", "original");
    for args in [
        vec!["format", &input, "--write", "--output", &destination],
        vec!["format", &input, "--write", "--check"],
        vec!["format", &input, "--check", "--output", &destination],
        vec!["format", &input, "--output", &input],
        vec!["format", "-", "--write"],
        vec!["format", &input, "--indent-width", "0"],
        vec!["format", &input, "--indent-width", "17"],
    ] {
        let output = run(&args, "");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "const x=1;");
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "original");
    }
}
