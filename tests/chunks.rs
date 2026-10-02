use jsexec::chunks::{
    Candidate, ChunkExtractor, Context, Engine, InputKind, Options, Report, Source,
};
use std::collections::BTreeSet;
use url::Url;

fn analyze(code: &str, extractors: &[&str]) -> Report {
    Engine::default()
        .analyze(
            &[Source {
                name: "bundle.js".into(),
                code: code.into(),
            }],
            &Options {
                extractors: extractors.iter().map(|name| name.to_string()).collect(),
                ..Options::default()
            },
        )
        .unwrap()
}
fn values(report: &Report) -> Vec<&str> {
    report
        .chunks
        .iter()
        .map(|chunk| chunk.value.as_str())
        .collect()
}

#[test]
fn discovers_imports_without_filename_heuristics() {
    let report = analyze(
        r#"import './main.js'; export * from './utils.mjs'; export { x } from './shared.cjs'; import('./login.js?x=1'); import(`./settings.js`); import(`./${name}.js`); import('react'); import('./style.css');"#,
        &["imports"],
    );
    assert_eq!(
        values(&report),
        [
            "./login.js?x=1",
            "./main.js",
            "./settings.js",
            "./shared.cjs",
            "./utils.mjs"
        ]
    );
    assert!(report.diagnostics.is_empty());
    assert!(
        report
            .chunks
            .iter()
            .all(|chunk| chunk.evidence[0].extractor == "imports")
    );
}

#[test]
fn literals_are_decoded_and_invalid_assets_are_filtered() {
    let report = analyze(
        r#"const paths = ['assets/\u0061pp.js', '/app.js?v=1#module', 'not a path.js', 'file:///tmp/app.js', 'data:text/app.js', 'javascript:app.js', './theme.css', './.js', './app.js/extra', './bad\\path.js', 'bare.js'];"#,
        &["references"],
    );
    assert_eq!(values(&report), ["/app.js?v=1#module", "assets/app.js"]);
}

#[test]
fn preserves_repeated_locations_and_groups_resolved_aliases() {
    let sources = [
        Source {
            name: "a.js".into(),
            code: "import('./x.js');\nimport('./x.js');".into(),
        },
        Source {
            name: "b.js".into(),
            code: "import('/assets/x.js');".into(),
        },
    ];
    let report = Engine::default()
        .analyze(
            &sources,
            &Options {
                base_url: Some(Url::parse("https://example.com/assets/main.js").unwrap()),
                extractors: BTreeSet::from(["imports".into()]),
                ..Options::default()
            },
        )
        .unwrap();
    assert_eq!(values(&report), ["https://example.com/assets/x.js"]);
    assert_eq!(report.chunks[0].evidence.len(), 3);
    assert_eq!(
        report.chunks[0].evidence[1].location.as_ref().unwrap().line,
        2
    );
    assert_eq!(
        report.chunks[0].evidence[1]
            .location
            .as_ref()
            .unwrap()
            .column,
        1
    );
}

#[test]
fn columns_count_unicode_characters_but_spans_count_bytes() {
    let report = analyze("const é = 'ñ'; import('./x.js');", &["imports"]);
    let location = report.chunks[0].evidence[0].location.as_ref().unwrap();
    assert_eq!(location.start, 17);
    assert_eq!(location.column, 16);
}

#[test]
fn vite_maps_are_found_in_parameter_defaults_and_preload_calls() {
    let report = analyze(
        r#"const __vite__mapDeps = (i,m=__vite__mapDeps,d=m.f||(m.f=['entry-XYZ.js','plain.js','style.css']))=>i.map(i=>d[i]); __vitePreload(()=>import('./a.js'),['dep.js']); const unrelated=['ignore.js'];"#,
        &["vite"],
    );
    assert_eq!(values(&report), ["dep.js", "entry-XYZ.js", "plain.js"]);
}

#[test]
fn webpack_concatenation_evaluates_name_fallback_and_public_path() {
    let report = analyze(
        r#"f.p='/cdn/'; f.u=e=>'static/js/'+({16:'login'}[e]||e)+'-'+{16:'abc',94:'def'}[e]+'.chunk.js';"#,
        &["webpack"],
    );
    assert_eq!(
        values(&report),
        [
            "/cdn/static/js/94-def.chunk.js",
            "/cdn/static/js/login-abc.chunk.js"
        ]
    );
}

#[test]
fn webpack_template_maps_do_not_mix_css_hashes() {
    let report = analyze(
        r#"b.u=e=>`index_${e}_${{99:'aaa',325:'bbb'}[e]}.js`; b.miniCssF=e=>`${e}.${{99:'ccc',424:'ddd'}[e]}.css`;"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["index_325_bbb.js", "index_99_aaa.js"]);
}

#[test]
fn webpack_uses_function_returns_computed_members_and_known_lazy_ids() {
    let report = analyze(
        r#"r['u']=function(id){return id+'.js'};r.e(42);r.e(9);other.e(3);"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["42.js", "9.js"]);
}

#[test]
fn webpack_conditional_branches_and_nullish_fallbacks() {
    let report = analyze(
        r#"r.u=id=>(id===7?'special':({8:'named'}[id]??id))+'.js';r.e(9);"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["9.js", "named.js", "special.js"]);
}

#[test]
fn webpack_numeric_addition_is_not_string_concatenation() {
    let report = analyze("r.u=id=>(id+1)+'.js';r.e(9);", &["webpack"]);
    assert_eq!(values(&report), ["10.js"]);
}

#[test]
fn webpack_does_not_invent_missing_map_values_or_execute_calls() {
    let report = analyze(
        r#"r.u=id=>({1:'a',2:'b'}[id])+'.'+{1:'hash'}[id]+'.js';s.u=id=>doSomething(id)+'.js';s.e(3);"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["a.hash.js"]);
}

#[test]
fn webpack_ignores_getters_and_parameter_mutations() {
    let report = analyze(
        r#"r.u=id=>({get 1(){return 'sideeffect'}}[id])+'.js'; s.u=function(id){id++;return id+'.js'};s.e(2);"#,
        &["webpack"],
    );
    assert!(report.chunks.is_empty());
}

#[test]
fn webpack_preserves_explicit_string_ids_and_leading_zero_map_keys() {
    let report = analyze(
        r#"r.u=id=>(id===9?'number':'string')+'-'+id+'.js';r.e('9');s.u=id=>id+'-'+{'01':'hash'}[id]+'.js';"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["01-hash.js", "string-9.js"]);
}

#[test]
fn webpack_requires_returns_and_avoids_ambiguous_public_paths() {
    let report = analyze(
        r#"r.p='/old/';r.p=dynamicPath;r.u=()=> 'fixed.js';s.u=function(id){'no-return.js'};t.u=id=>{'also-no-return.js'};"#,
        &["webpack"],
    );
    assert_eq!(values(&report), ["fixed.js"]);
}

#[test]
fn manifests_handle_nested_strings_and_protocol_relative_urls() {
    let report = Engine::default().analyze(&[Source { name: "manifest.json".into(), code: r#"{"chunks":[" //cdn.example.com/x.js ",{"file":"./x.mjs#module"},"./style.css"]}"#.into() }], &Options {
        base_url: Some(Url::parse("https://example.com/assets/main.js").unwrap()), ..Options::default()
    }).unwrap();
    assert_eq!(
        values(&report),
        [
            "https://cdn.example.com/x.js",
            "https://example.com/assets/x.mjs#module"
        ]
    );
    assert!(report.chunks[0].evidence[0].location.is_none());
}

#[test]
fn html_extracts_scripts_preloads_and_json_manifests() {
    let code = r#"<SCRIPT SRC='/main.js?a=1&amp;b=2'></SCRIPT><link rel='modulepreload' href='./lazy.mjs'><link rel='preload' as='script' href='./pre.js'><link rel='stylesheet' href='./theme.js'><script type='application/json; charset=utf-8'>{"chunks":["//cdn.example.com/vendor.js"]}</script>"#;
    let report = analyze(code, &[]);
    assert_eq!(
        values(&report),
        [
            "./lazy.mjs",
            "./pre.js",
            "//cdn.example.com/vendor.js",
            "/main.js?a=1&b=2"
        ]
    );
    assert!(report.diagnostics.is_empty());
}

#[test]
fn malformed_sources_report_diagnostics() {
    let report = analyze("import('./valid.js'); return unexpected;", &["imports"]);
    assert!(!report.diagnostics.is_empty());
    let report = Engine::default()
        .analyze(
            &[Source {
                name: "bad.json".into(),
                code: "{invalid".into(),
            }],
            &Options::default(),
        )
        .unwrap();
    assert!(!report.diagnostics.is_empty());
}

#[test]
fn invalid_base_and_unknown_extractors_fail() {
    for base in ["file:///tmp/", "data:text/plain,bundle"] {
        assert!(
            Engine::default()
                .analyze(
                    &[],
                    &Options {
                        base_url: Some(Url::parse(base).unwrap()),
                        ..Options::default()
                    }
                )
                .is_err()
        );
    }
    assert!(
        Engine::default()
            .analyze(
                &[],
                &Options {
                    extractors: BTreeSet::from(["unknown".into()]),
                    ..Options::default()
                }
            )
            .is_err()
    );
}

#[test]
fn custom_extractors_share_validation_resolution_and_evidence() {
    struct Custom;
    impl ChunkExtractor for Custom {
        fn name(&self) -> &'static str {
            "custom"
        }
        fn extract(&self, _: &Context<'_>, candidates: &mut Vec<Candidate>) {
            candidates.push(Candidate::new("custom.js", None));
            candidates.push(Candidate::new("not-js.css", None));
        }
    }
    let mut engine = Engine::new();
    engine.register(Custom);
    let report = engine
        .analyze(
            &[Source {
                name: "input.txt".into(),
                code: "".into(),
            }],
            &Options {
                input_kind: InputKind::JavaScript,
                ..Options::default()
            },
        )
        .unwrap();
    assert_eq!(values(&report), ["custom.js"]);
    assert_eq!(report.chunks[0].evidence[0].extractor, "custom");
}
