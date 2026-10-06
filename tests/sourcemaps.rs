use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use jsexec::source::Source;
use jsexec::sourcemaps::{self, InputKind, MapKind, Options, Report};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::json;
use url::Url;

fn analyze(name: &str, code: &str, base: Option<&str>) -> Report {
    sourcemaps::analyze(
        &[Source {
            name: name.into(),
            code: code.into(),
        }],
        &Options {
            base_url: base.map(|base| Url::parse(base).unwrap()),
            ..Options::default()
        },
    )
    .unwrap()
}
fn basic_map() -> String {
    json!({"version":3,"file":"bundle.js","sources":["src/index.ts","empty.js","missing.js",null],"sourcesContent":["const greeting = 'ñ';","",null,"anonymous"],"ignoreList":[1],"mappings":""}).to_string()
}

#[test]
fn saved_maps_preserve_empty_missing_and_anonymous_sources() {
    let report = analyze(
        "app.map",
        &basic_map(),
        Some("https://example.com/assets/app.js.map"),
    );
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.maps[0].kind, MapKind::File);
    assert_eq!(report.maps[0].generated_file.as_deref(), Some("bundle.js"));
    assert_eq!(report.embedded_count(), 3);
    let sources = &report.maps[0].sources;
    assert_eq!(sources.len(), 4);
    assert_eq!(
        sources[0].resolved_url.as_deref(),
        Some("https://example.com/assets/src/index.ts")
    );
    assert_eq!(sources[1].content.as_deref(), Some(""));
    assert!(sources[1].ignored);
    assert!(sources[2].content.is_none());
    assert!(sources[3].path.is_none());
    assert_eq!(sources[3].content.as_deref(), Some("anonymous"));
}

#[test]
fn base64_percent_encoded_and_unpadded_inline_maps_decode() {
    let map = basic_map();
    let uris = [
        format!(
            "data:application/json;charset=utf-8;base64,{}",
            STANDARD.encode(&map)
        ),
        format!(
            "data:application/json;base64,{}",
            STANDARD_NO_PAD.encode(&map)
        ),
        format!(
            "data:application/json,{}",
            utf8_percent_encode(&map, NON_ALPHANUMERIC)
        ),
        format!(
            "data:application/octet-stream;base64,{}",
            utf8_percent_encode(&STANDARD.encode(&map), NON_ALPHANUMERIC)
        ),
    ];
    for uri in uris {
        let report = analyze(
            "inline.js",
            &format!("const bundle = 1;\n//# sourceMappingURL={uri}"),
            Some("https://example.com/assets/inline.js"),
        );
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(report.maps[0].kind, MapKind::Inline);
        assert!(report.maps[0].reference.is_none());
        assert_eq!(report.embedded_count(), 3);
        assert_eq!(
            report.maps[0].sources[0].content.as_deref(),
            Some("const greeting = 'ñ';")
        );
        assert_eq!(report.maps[0].location.as_ref().unwrap().line, 2);
    }
}

#[test]
fn percent_encoded_maps_preserve_literal_plus_signs() {
    let code = "//# sourceMappingURL=data:application/json,%7B%22version%22:3,%22sources%22:[%22a.js%22],%22sourcesContent%22:[%221+2%22]%7D";
    assert_eq!(
        analyze("inline.js", code, None).maps[0].sources[0]
            .content
            .as_deref(),
        Some("1+2")
    );
}

#[test]
fn final_annotation_is_authoritative() {
    for code in [
        "//# sourceMappingURL=first.map\n//# sourceMappingURL=../maps/final.map?v=1#x\r\n",
        "//@sourceMappingURL=../maps/final.map?v=1#x",
        "/*# sourceMappingURL=../maps/final.map?v=1#x */",
    ] {
        let report = analyze(
            "script.js",
            code,
            Some("https://example.com/assets/script.js?old=1"),
        );
        assert!(report.diagnostics.is_empty());
        assert_eq!(report.maps.len(), 1);
        assert_eq!(
            report.maps[0].reference.as_deref(),
            Some("../maps/final.map?v=1#x")
        );
        assert_eq!(
            report.maps[0].url.as_deref(),
            Some("https://example.com/maps/final.map?v=1#x")
        );
    }
    assert!(
        analyze(
            "script.js",
            "//# sourceMappingURL=first.map\n//# sourceMappingURL=",
            None
        )
        .maps
        .is_empty()
    );
}

#[test]
fn comment_like_strings_templates_regexes_and_ordinary_comments_are_ignored() {
    let code = r#"const a="//# sourceMappingURL=fake.map";const b=`/*# sourceMappingURL=fake.map */`;const c=/\/\/# sourceMappingURL=fake.map/; // sourceMappingURL=ordinary.map
//# sourceMappingURL=real.map
"#;
    let report = analyze("script.js", code, None);
    assert_eq!(report.maps.len(), 1);
    assert_eq!(report.maps[0].reference.as_deref(), Some("real.map"));
    assert!(report.maps[0].url.is_none());
}

#[test]
fn javascript_extension_wins_over_json_like_content() {
    let report = analyze(
        "block.js",
        "{const x = 1;}\n//# sourceMappingURL=block.js.map",
        None,
    );
    assert_eq!(report.maps[0].kind, MapKind::External);
}

#[test]
fn protocol_relative_and_absolute_map_urls_resolve() {
    for (reference, expected) in [
        (
            "//cdn.example.com/app.map",
            "https://cdn.example.com/app.map",
        ),
        (
            "https://cdn.example.com/app.map",
            "https://cdn.example.com/app.map",
        ),
        ("/maps/app.map", "https://example.com/maps/app.map"),
    ] {
        assert_eq!(
            analyze(
                "script.js",
                &format!("//# sourceMappingURL={reference}"),
                Some("https://example.com/assets/script.js")
            )
            .maps[0]
                .url
                .as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn source_roots_are_directory_prefixes_and_virtual_urls_are_preserved() {
    for root in ["../src", "../src/"] {
        let map =
            json!({"version":3,"sourceRoot":root,"sources":["app.ts"],"sourcesContent":["source"]});
        assert_eq!(
            analyze(
                "app.map",
                &map.to_string(),
                Some("https://example.com/maps/app.map")
            )
            .maps[0]
                .sources[0]
                .resolved_url
                .as_deref(),
            Some("https://example.com/src/app.ts")
        );
    }
    let report = analyze("app.map", &json!({"version":3,"sources":["webpack://project/./src/main.ts"],"sourcesContent":["source"]}).to_string(), Some("https://example.com/app.map"));
    assert!(
        report.maps[0].sources[0]
            .resolved_url
            .as_deref()
            .unwrap()
            .starts_with("webpack://")
    );
}

#[test]
fn indexed_maps_flatten_nested_sections_without_inheriting_parent_roots() {
    let map = json!({"version":3,"sourceRoot":"/parent","sections":[
        {"offset":{"line":0,"column":0},"map":{"version":3,"sources":["a.ts"],"sourcesContent":["one"]}},
        {"offset":{"line":10,"column":0},"map":{"version":3,"sections":[{"offset":{"line":0,"column":0},"map":{"version":3,"sourceRoot":"/child","sources":["a.ts"],"sourcesContent":["two"]}}]}},
        {"url":"sections/external.map"}
    ]});
    let report = analyze(
        "indexed.map",
        &map.to_string(),
        Some("https://example.com/maps/indexed.map"),
    );
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.maps[0].sources.len(), 2);
    assert_eq!(report.maps[0].sources[0].section, [0]);
    assert_eq!(report.maps[0].sources[1].section, [1, 0]);
    assert_eq!(
        report.maps[0].sources[0].resolved_url.as_deref(),
        Some("https://example.com/maps/a.ts")
    );
    assert_eq!(
        report.maps[0].sources[1].resolved_url.as_deref(),
        Some("https://example.com/child/a.ts")
    );
    assert_eq!(
        report.maps[0].external_sections[0].url.as_deref(),
        Some("https://example.com/maps/sections/external.map")
    );
}

#[test]
fn mismatched_and_invalid_entries_keep_indices_and_report_diagnostics() {
    let report = analyze(
        "bad.map",
        &json!({"version":3,"sources":["a.ts",42,"missing.ts"],"sourcesContent":["one",99]})
            .to_string(),
        None,
    );
    assert_eq!(report.maps[0].sources.len(), 3);
    assert_eq!(report.maps[0].sources[2].index, 2);
    assert!(report.maps[0].sources[2].content.is_none());
    assert_eq!(report.diagnostics.len(), 3);
}

#[test]
fn malformed_maps_and_invalid_inline_encodings_return_diagnostics() {
    for json in [
        "{broken",
        "[]",
        r#"{"version":2,"sources":[]}"#,
        r#"{"version":3}"#,
        r#"{"version":3,"sections":42}"#,
    ] {
        assert!(
            !analyze("bad.map", json, None).diagnostics.is_empty(),
            "{json}"
        );
    }
    for uri in [
        "data:application/json;base64,!!!",
        "data:application/json,%GG",
        "data:application/json;base64,/w==",
        "data:application/json;base64",
    ] {
        let report = analyze("script.js", &format!("//# sourceMappingURL={uri}"), None);
        assert!(!report.diagnostics.is_empty(), "{uri}");
        assert_eq!(report.embedded_count(), 0);
    }
}

#[test]
fn unsupported_external_schemes_and_invalid_bases_fail_visibly() {
    let report = analyze(
        "script.js",
        "//# sourceMappingURL=file:///tmp/app.map",
        None,
    );
    assert!(!report.diagnostics.is_empty());
    assert!(report.maps[0].url.is_none());
    assert!(
        sourcemaps::analyze(
            &[],
            &Options {
                base_url: Some(Url::parse("file:///tmp/").unwrap()),
                input_kind: InputKind::Auto
            }
        )
        .is_err()
    );
}

#[test]
fn utf8_bom_maps_and_multiple_inputs_are_supported() {
    let sources = [
        Source {
            name: "a.map".into(),
            code: format!("\u{feff}{}", basic_map()),
        },
        Source {
            name: "b.js".into(),
            code: "//# sourceMappingURL=b.map".into(),
        },
    ];
    let report = sourcemaps::analyze(&sources, &Options::default()).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.maps.len(), 2);
    assert_eq!(report.maps[1].input, "b.js");
}

#[test]
fn response_headers_override_inline_annotations_without_discarded_diagnostics() {
    let source = Source {
        name: "https://example.com/app.js?v=1".into(),
        code: "const a = 1;\n//# sourceMappingURL=data:application/json;base64,broken".into(),
    };
    let options = Options {
        base_url: Some(Url::parse("https://example.com/app.js?v=1").unwrap()),
        ..Options::default()
    };
    let report = sourcemaps::analyze_response(&source, &options, Some("app.map")).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.maps[0].kind, MapKind::External);
    assert_eq!(
        report.maps[0].url.as_deref(),
        Some("https://example.com/app.map")
    );
    assert!(report.maps[0].location.is_none());
}

#[test]
fn remote_filenames_ignore_queries_for_format_detection() {
    let report = analyze(
        "https://example.com/app.js?build=1",
        "{ const a = 1; }",
        None,
    );
    assert!(report.diagnostics.is_empty());
    assert!(report.maps.is_empty());
}

#[test]
fn response_header_selects_javascript_for_extensionless_blocks_and_reports_empty_values() {
    let source = Source {
        name: "https://example.com/script".into(),
        code: "{ const answer = 42; }".into(),
    };
    let options = Options {
        base_url: Some(Url::parse(&source.name).unwrap()),
        ..Options::default()
    };
    let report = sourcemaps::analyze_response(&source, &options, Some("app.map")).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.maps[0].kind, MapKind::External);
    let report = sourcemaps::analyze_response(&source, &options, Some(" ")).unwrap();
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].message.contains("header is empty"));
}

#[test]
fn unused_map_fields_are_skipped_but_their_json_is_still_validated() {
    let map = format!(
        r#"{{"version":3,"sources":["a.js","empty.js"],"sourcesContent":["content",""],"mappings":"{}","names":["unused"],"vendor":{{"unused":"large"}}}}"#,
        "AAAA;".repeat(100_000)
    );
    let report = analyze("app.map", &map, None);
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.embedded_count(), 2);
    assert_eq!(
        report.maps[0].sources[0].content.as_deref(),
        Some("content")
    );
    assert_eq!(report.maps[0].sources[1].content.as_deref(), Some(""));
    let report = analyze(
        "bad.map",
        r#"{"version":3,"sources":[],"mappings":"bad\q"}"#,
        None,
    );
    assert!(report.maps[0].sources.is_empty());
    assert!(
        report.diagnostics[0]
            .message
            .contains("invalid source map JSON")
    );
}

#[test]
fn borrowed_map_fields_preserve_last_duplicate_values_and_partial_recovery() {
    let report = analyze(
        "app.map",
        r#"{"version":2,"version":3,"sources":["old"],"sources":["a.js",null,"bad.js"],"sourcesContent":["one","",42],"ignoreList":["invalid",0],"unused":true}"#,
        None,
    );
    assert_eq!(report.maps[0].sources.len(), 3);
    assert_eq!(report.maps[0].sources[0].content.as_deref(), Some("one"));
    assert!(report.maps[0].sources[0].ignored);
    assert_eq!(report.maps[0].sources[1].content.as_deref(), Some(""));
    assert!(report.maps[0].sources[2].content.is_none());
    assert_eq!(report.diagnostics.len(), 1);
}
