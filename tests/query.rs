use jsexec::query::{QueryIndex, Relation, RenderOptions, Selector};
use jsexec::source::Source;

fn source() -> Source {
    Source {
        name: "example.ts".into(),
        code: r#"
const config = {base: "/api", token: "secret"};
async function load(id: number) {
  const route = `/api/${id}`;
  return fetch(route, {method: "POST", headers: {Authorization: config.token}});
}
const other = () => client.get("/public", {params: {page: 2}});
fetch("/health");
"#
        .into(),
    }
}
fn select<'a>(index: &'a QueryIndex<'a>, query: &str) -> jsexec::query::Page {
    index
        .select(
            &Selector::parse(query).unwrap(),
            0,
            100,
            &RenderOptions::default(),
        )
        .unwrap()
}

#[test]
fn queries_arbitrary_node_kinds_and_nested_fields() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    let calls = select(&index, r#"CallExpression[callee.name="fetch"]"#);
    assert_eq!(calls.match_count, 2);
    let call = select(
        &index,
        r#"CallExpression[callee.property.name="get"][arguments.0.value="/public"]"#,
    );
    assert_eq!(call.match_count, 1);
    assert!(call.nodes[0].code.starts_with("client.get"));
    assert_eq!(select(&index, "TSNumberKeyword").match_count, 1);
    assert_eq!(
        select(&index, "FunctionDeclaration[async=true]").match_count,
        1
    );
}

#[test]
fn selectors_match_decoded_literals_wildcards_numbers_and_regexes() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(select(&index, r#"Literal[value^="/api"]"#).match_count, 1);
    assert_eq!(select(&index, r#"Literal[value=/^POST$/i]"#).match_count, 1);
    assert_eq!(select(&index, "Literal[value>=2]").match_count, 1);
    assert_eq!(select(&index, "Literal[value=2.0]").match_count, 1);
    assert_eq!(
        select(&index, "CallExpression[arguments.length>1]").match_count,
        2
    );
    assert_eq!(
        select(&index, r#"CallExpression[arguments.*.value="/health"]"#).match_count,
        1
    );
    assert_eq!(
        select(&index, "CallExpression[nonexistent!=null]").match_count,
        0
    );
}

#[test]
fn contains_operators_and_wildcard_field_paths_are_unambiguous() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(select(&index, "Literal[value*='api']").match_count, 1);
    assert_eq!(
        select(&index, "CallExpression[@text*='fetch']").match_count,
        2
    );
    assert_eq!(
        select(&index, "CallExpression[arguments.*.value*='/']").match_count,
        2
    );
    assert_eq!(
        select(&index, "Identifier[*='Identifier']").match_count,
        select(&index, "Identifier").match_count
    );
}

#[test]
fn parent_descendant_and_edge_field_selectors_use_actual_ast_relations() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(
        select(
            &index,
            r#"FunctionDeclaration CallExpression[callee.name="fetch"]"#
        )
        .match_count,
        1
    );
    assert_eq!(
        select(&index, "CallExpression > Identifier.callee").match_count,
        2
    );
    assert_eq!(
        select(&index, "FunctionDeclaration > Identifier.params").match_count,
        1
    );
    assert_eq!(
        select(&index, "FunctionDeclaration > Identifier.params.*").match_count,
        1
    );
    assert_eq!(
        select(&index, "FunctionDeclaration > CallExpression").match_count,
        0
    );
    assert_eq!(
        select(&index, "Identifier[name='load'], Identifier[name='load']").match_count,
        1
    );
}

#[test]
fn has_not_is_and_direct_has_are_composable() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(
        select(
            &index,
            r#"FunctionDeclaration:has(CallExpression[callee.name="fetch"])"#
        )
        .match_count,
        1
    );
    assert_eq!(
        select(&index, "CallExpression:has(> Identifier.callee)").match_count,
        2
    );
    assert_eq!(
        select(&index, "FunctionDeclaration:has(> CallExpression)").match_count,
        0
    );
    assert_eq!(
        select(&index, "CallExpression:not([callee.name='fetch'])").match_count,
        1
    );
    assert_eq!(
        select(&index, ":is(FunctionDeclaration, ArrowFunctionExpression)").match_count,
        2
    );
    assert_eq!(
        select(
            &index,
            "Program:has(FunctionDeclaration:not([async=false]))"
        )
        .match_count,
        1
    );
}

#[test]
fn navigation_uses_node_ids_and_preserves_edge_paths() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    let calls = select(&index, "FunctionDeclaration CallExpression");
    let call = &calls.nodes[0];
    let options = RenderOptions::default();
    let children = index
        .related(&call.id, Relation::Children, 0, 100, &options)
        .unwrap();
    assert_eq!(
        children
            .nodes
            .iter()
            .map(|n| n.field.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["callee", "arguments.0", "arguments.1"]
    );
    let parent = index
        .related(&call.id, Relation::Parent, 0, 100, &options)
        .unwrap();
    assert_eq!(parent.nodes[0].kind, "ReturnStatement");
    let context = index
        .related(&call.id, Relation::Context, 0, 100, &options)
        .unwrap();
    assert_eq!(context.nodes[0].kind, "FunctionDeclaration");
    assert!(context.nodes[0].code.contains("const route"));
    let ancestors = index
        .related(&call.id, Relation::Ancestors, 0, 100, &options)
        .unwrap();
    assert_eq!(ancestors.nodes[0].id, parent.nodes[0].id);
    assert_eq!(ancestors.nodes.last().unwrap().id, index.root_id());
    let descendants = index
        .related(&call.id, Relation::Descendants, 0, 100, &options)
        .unwrap();
    assert!(descendants.nodes.iter().any(|n| n.code == "Authorization"));
    let siblings = index
        .related(&children.nodes[0].id, Relation::Siblings, 0, 100, &options)
        .unwrap();
    assert_eq!(siblings.match_count, 2);
    assert_eq!(
        index
            .related(&index.root_id(), Relation::Parent, 0, 1, &options)
            .unwrap()
            .match_count,
        0
    );
    assert!(
        index
            .related("n9999999", Relation::SelfNode, 0, 1, &options)
            .is_err()
    );
}

#[test]
fn projection_and_truncation_are_explicit_and_do_not_affect_matching() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    let options = RenderOptions {
        max_code: 5,
        max_value: 2,
        max_items: 1,
        fields: vec!["callee.name".into(), "arguments".into(), "@text".into()],
    };
    let page = index
        .select(
            &Selector::parse("CallExpression[callee.name='fetch']").unwrap(),
            0,
            1,
            &options,
        )
        .unwrap();
    assert_eq!(page.match_count, 2);
    let node = &page.nodes[0];
    assert_eq!(node.code, "fetch");
    assert!(node.code_truncated);
    assert_eq!(node.fields["callee.name"], "fe");
    assert_eq!(node.fields["arguments"].as_array().unwrap().len(), 1);
    assert!(node.fields["arguments"][0]["node"].is_string());
    assert!(node.truncated_fields.contains(&"arguments".into()));
    assert!(node.truncated_fields.contains(&"callee.name".into()));
    assert!(node.truncated_fields.contains(&"@text".into()));
}

#[test]
fn pagination_and_count_only_queries_are_deterministic() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    let selector = Selector::parse("CallExpression").unwrap();
    let options = RenderOptions::default();
    let page = index.select(&selector, 0, 1, &options).unwrap();
    assert_eq!(page.match_count, 3);
    assert_eq!(page.next_offset, Some(1));
    let second = index.select(&selector, 1, 1, &options).unwrap();
    assert!(second.nodes[0].code.starts_with("client.get"));
    let count = index.select(&selector, 0, 0, &options).unwrap();
    assert!(count.nodes.is_empty());
    assert!(count.has_more);
    assert_eq!(count.next_offset, None);
    assert!(!index.select(&selector, 100, 1, &options).unwrap().has_more);
    let again = QueryIndex::parse(&source).unwrap();
    assert_eq!(again.hash, index.hash);
    assert_eq!(
        select(&again, "*")
            .nodes
            .iter()
            .map(|n| &n.id)
            .collect::<Vec<_>>(),
        select(&index, "*")
            .nodes
            .iter()
            .map(|n| &n.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn supports_jsx_typescript_patterns_and_non_string_literals() {
    let source = Source {
        name: "example.tsx".into(),
        code: r#"
const {x: alias, ...rest} = object;
const value: string = "café 🌟";
const big = 123n; const pattern = /api/i;
const view = <Widget prop={value} />;
"#
        .into(),
    };
    let index = QueryIndex::parse(&source).unwrap();
    assert!(index.diagnostics.is_empty());
    for kind in [
        "ObjectPattern",
        "RestElement",
        "TSStringKeyword",
        "JSXElement",
    ] {
        assert_eq!(select(&index, kind).match_count, 1, "{kind}");
    }
    assert_eq!(select(&index, "Literal[bigint='123']").match_count, 1);
    assert_eq!(
        select(&index, "Literal[regex.pattern='api'][regex.flags='i']").match_count,
        1
    );
    let literal = &select(&index, "Literal[value='café 🌟']").nodes[0];
    assert_eq!(
        &source.code[literal.location.start as usize..literal.location.end as usize],
        literal.code
    );
    let kinds = index.kinds();
    assert!(
        kinds
            .iter()
            .any(|k| k.kind == "JSXElement" && k.fields.contains(&"openingElement".into()))
    );
}

#[test]
fn indexes_deep_trees_without_json_recursion_limit() {
    let source = Source {
        name: "deep.js".into(),
        code: format!("let value = {};", vec!["x"; 400].join(" + ")),
    };
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(select(&index, "BinaryExpression").match_count, 399);
}

#[test]
fn rejects_invalid_and_unsupported_selectors() {
    for query in [
        "",
        "CallExpression[",
        "Identifier[name='x]",
        "[value=/abc]",
        "[value=/x/g]",
        "[value~=3]",
        "CallExpression + Identifier",
        ":unknown(x)",
        ":has(CallExpression > Identifier)",
        ":is()",
        "CallExpression,",
        "[@unknown=1]",
        "[a..b=1]",
        "Identifier.params..0",
        "Identifier.params.a*b",
        "[arguments.a*b=1]",
    ] {
        assert!(Selector::parse(query).is_err(), "accepted {query}");
    }
}

#[test]
fn field_projections_have_stable_cardinality_and_metadata_matches() {
    let source = source();
    let index = QueryIndex::parse(&source).unwrap();
    let selector = Selector::parse("CallExpression[callee.name='fetch']").unwrap();
    let options = RenderOptions {
        fields: vec!["arguments.*.type".into(), "missing".into(), "@start".into()],
        ..RenderOptions::default()
    };
    let page = index.select(&selector, 0, 20, &options).unwrap();
    assert_eq!(
        page.nodes[0].fields["arguments.*.type"],
        serde_json::json!(["Identifier", "ObjectExpression"])
    );
    assert_eq!(
        page.nodes[1].fields["arguments.*.type"],
        serde_json::json!(["Literal"])
    );
    for node in &page.nodes {
        assert_eq!(node.fields["missing"], serde_json::Value::Null);
        assert_eq!(node.fields["@start"], node.location.start);
        assert_eq!(
            select(&index, &format!("[@id='{}']", node.id)).match_count,
            1
        );
    }
    assert_eq!(
        select(&index, "CallExpression[@text^='client.get'][@line>=7]").match_count,
        1
    );
    assert_eq!(
        select(&index, "FunctionDeclaration[async][async!=false]").match_count,
        1
    );
}

#[test]
fn selector_limits_fail_with_actionable_errors() {
    assert!(
        Selector::parse(&"*".repeat(16_385))
            .err()
            .unwrap()
            .to_string()
            .contains("16384")
    );
    assert!(
        Selector::parse(&vec!["*"; 65].join(" "))
            .err()
            .unwrap()
            .to_string()
            .contains("64 compound")
    );
    let nested = format!("{}Identifier{}", ":not(".repeat(17), ")".repeat(17));
    assert!(
        Selector::parse(&nested)
            .err()
            .unwrap()
            .to_string()
            .contains("nesting exceeds 16")
    );
}

#[test]
fn syntax_diagnostics_are_retained() {
    let source = Source {
        name: "bad.js".into(),
        code: "const = ;".into(),
    };
    assert!(!QueryIndex::parse(&source).unwrap().diagnostics.is_empty());
}

#[test]
fn unusual_javascript_strings_do_not_prevent_node_access() {
    let source = Source {
        name: "strings.js".into(),
        code: r#"const s = "a\uD800\n🌟";"#.into(),
    };
    let index = QueryIndex::parse(&source).unwrap();
    assert_eq!(select(&index, "Literal").match_count, 1);
    let literal = &select(&index, "Literal").nodes[0];
    assert_eq!(literal.code, r#""a\uD800\n🌟""#);
    assert_eq!(literal.fields["value"]["encoding"], "utf16");
    assert_eq!(
        literal.fields["value"]["units"],
        serde_json::json!([97, 55296, 10, 55356, 57119])
    );
    assert_eq!(
        select(&index, "Literal[value.units.1=55296]").match_count,
        1
    );
}

#[test]
fn compact_index_remains_send_and_sync_for_library_consumers() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<QueryIndex<'static>>();
}
