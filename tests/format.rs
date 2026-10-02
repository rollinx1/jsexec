use jsexec::format::{self, InputKind, Options};
use jsexec::source::Source;
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

fn formatted(name: &str, code: &str) -> String {
    format::format(
        &Source {
            name: name.into(),
            code: code.into(),
        },
        &Options::default(),
    )
    .unwrap()
}

#[test]
fn beautifies_javascript_without_renaming_or_rewriting_logic() {
    let output = formatted(
        "app.js",
        "function load(id){const path='/api/'+id;return fetch(path,{method:'POST'});}load(3);",
    );
    assert!(output.contains("function load(id) {\n  const path = \"/api/\" + id;"));
    assert!(output.contains("return fetch(path, { method: \"POST\" });"));
    assert!(output.ends_with('\n'));
    assert_eq!(formatted("app.js", &output), output);
}

#[test]
fn preserves_typed_syntax_and_selects_jsx_modes_by_filename() {
    for (name, code, kind) in [
        (
            "app.ts",
            "const identity=<T>(value:T):T=>value;",
            SourceType::ts(),
        ),
        (
            "app.tsx",
            "const element: JSX.Element=<Widget value={3}/>;",
            SourceType::tsx(),
        ),
        (
            "app.jsx",
            "export const App=()=> <div>Hello <b>world</b>!</div>;",
            SourceType::jsx(),
        ),
        (
            "app.mjs",
            "export const x=await Promise.resolve(1);",
            SourceType::mjs(),
        ),
        (
            "app.cjs",
            "module.exports=function(){return require('path')};",
            SourceType::cjs(),
        ),
    ] {
        let output = formatted(name, code);
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &output, kind).parse();
        assert!(parsed.errors.is_empty(), "{name}: {:?}", parsed.errors);
        assert_eq!(formatted(name, &output), output);
        if name.ends_with("ts") {
            assert!(output.contains(": T"));
        }
        if name.ends_with("tsx") {
            assert!(output.contains("JSX.Element"));
        }
    }
}

#[test]
fn retains_hashbang_licenses_statement_comments_and_import_annotations() {
    let code = "#!/usr/bin/env node\n/*! @license MIT */\n// load lazily\nconst load=()=>import(/* webpackChunkName: 'login' */ './login.js');\n";
    let output = formatted("app.js", code);
    for text in [
        "#!/usr/bin/env node",
        "@license MIT",
        "load lazily",
        "webpackChunkName",
        "./login.js",
    ] {
        assert!(output.contains(text), "missing {text}: {output}");
    }
}

#[test]
fn does_not_damage_literals_or_automatic_semicolon_insertion() {
    let code = r#"const s="\uD800"; const re=/a\/b/gi; const t=` /api/${id}\n `; function f(){return
{value:1}}"#;
    let output = formatted("app.js", code);
    for text in [r#"\ud800"#, r#"/a\/b/gi"#, "return;"] {
        assert!(output.contains(text), "{output}");
    }
    assert_eq!(formatted("app.js", &output), output);
}

#[test]
fn options_control_quote_style_indentation_and_input_mode() {
    let source = Source {
        name: "saved.txt".into(),
        code: "function f(){const x: number=1;return \"hello\";}".into(),
    };
    let options = Options {
        input_kind: InputKind::TypeScript,
        single_quote: true,
        indent_width: 4,
    };
    let output = format::format(&source, &options).unwrap();
    assert!(output.contains("\n    const x: number = 1;"));
    assert!(output.contains("return 'hello';"));
    assert!(format::format(&source, &Options::default()).is_err());
    assert!(
        format::format(
            &source,
            &Options {
                indent_width: 0,
                ..options
            }
        )
        .is_err()
    );
}

#[test]
fn javascript_parse_errors_fail_instead_of_printing_recovered_nodes() {
    for code in ["const = 3;", "function f( {", "const typed: number=1;"] {
        let source = Source {
            name: "bad.js".into(),
            code: code.into(),
        };
        assert!(
            format::format(&source, &Options::default()).is_err(),
            "{code}"
        );
    }
}

#[test]
fn html_indents_structural_containers_and_is_idempotent() {
    let output = formatted(
        "page.html",
        "<!doctype html><html><head><title>Hello</title></head><body><main><p>One</p><p>Two</p></main></body></html>",
    );
    assert!(
        output.starts_with("<!DOCTYPE html>\n<html>\n  <head>\n    <title>Hello</title>"),
        "{output}"
    );
    assert!(
        output.contains("\n    <main>\n      <p>One</p>\n      <p>Two</p>\n    </main>"),
        "{output}"
    );
    assert_eq!(formatted("page.html", &output), output);
}

#[test]
fn html_preserves_inline_spacing_nonbreaking_spaces_and_attribute_values() {
    let output = formatted(
        "page.htm",
        r#"<p title="A &quot;quote&quot; &amp; stuff"> Hello  <span>world &lt;x&gt;</span> ! </p><div>&nbsp;</div><img src="a&amp;b">"#,
    );
    assert!(output.contains(r#"<p title="A &quot;quote&quot; &amp; stuff"> Hello  <span>world &lt;x&gt;</span> ! </p>"#), "{output}");
    assert!(output.contains("<div>&nbsp;</div>"));
    assert!(output.contains("<img src=\"a&amp;b\">"));
    assert!(!output.contains("</img>"));
    assert_eq!(formatted("page.htm", &output), output);
}

#[test]
fn html_preserves_raw_and_preformatted_content_templates_and_namespaces() {
    let code = r##"<pre>

  a  <span>b</span>
</pre><textarea>

 keep  &lt;x&gt;
</textarea><script type="application/json"> {"key": " a  b "} </script><style> p::before { content: " a  b "; } </style><template><p>Inside</p></template><svg viewBox="0 0 10 10"><use xlink:href="#a" /></svg>"##;
    let output = formatted("page.html", code);
    for text in [
        "<pre>\n\n  a  <span>b</span>\n</pre>",
        "<textarea>\n\n keep  &lt;x&gt;\n</textarea>",
        r#" {"key": " a  b "} "#,
        r#" p::before { content: " a  b "; } "#,
        "<p>Inside</p>",
        "viewBox=\"0 0 10 10\"",
        "xlink:href=\"#a\"",
    ] {
        assert!(output.contains(text), "missing {text}: {output}");
    }
    assert_eq!(formatted("page.html", &output), output);
}

#[test]
fn html_retains_legacy_doctype_identifiers() {
    let code = r#"<!DOCTYPE html PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN" "http://www.w3.org/TR/html4/loose.dtd"><p>x</p>"#;
    assert!(formatted("page.html", code).starts_with(code.split("<p>").next().unwrap()));
}

#[test]
fn html_document_boundaries_are_stable_for_empty_and_mixed_bodies() {
    for code in [
        "<!doctype html><html><head></head><body></body></html>",
        "<!doctype html><html><head></head><body><img src='x'> tail  </body></html><!-- after -->",
        "<!-- before --><html><head><title>x</title></head><body><span>a</span> <span>b</span></body></html><!-- after -->",
    ] {
        let output = formatted("page.html", code);
        assert_eq!(formatted("page.html", &output), output, "{code}");
    }
}

#[test]
fn html_rejects_plaintext_instead_of_appending_closing_tags_to_its_content() {
    let source = Source {
        name: "page.html".into(),
        code: "<plaintext>raw content".into(),
    };
    assert!(format::format(&source, &Options::default()).is_err());
}
