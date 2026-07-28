//! RFC-0080 compiler-owned identities for module-qualified type and pattern builders.

use witchy::{interpreter, parser, pipeline, typeck};

const DEFINITION_SUPPORT: &str = r#"
type Wrapped(a):
    Wrapped(a)

pub fn answer() -> Int:
    42
"#;

const CONSUMER_SUPPORT: &str = r#"
type Wrapped(a):
    Wrapped(a)
"#;

const TAG_LIBRARY: &str = r#"
import meta
import definition_support

pub comptime fn definition_qualified(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let argument = quote type:
        Int
    let ty = meta.type_qualified(meta.ident("definition_support"), meta.ident("Wrapped"), [argument])
    let binding = meta.pattern_var(meta.ident("value"))
    let pattern = meta.pattern_qualified_ctor(meta.ident("definition_support"), meta.ident("Wrapped"), [binding])
    let item = meta.expr_name(meta.ident("item"))
    let body = quote expr:
        value + 1
    let matched = meta.expr_match(item, [meta.match_arm(pattern, body)])
    quote expr:
        (fn(item: ${ty}) -> Int: ${matched})(definition_support.Wrapped(41))

pub comptime fn callsite_qualified(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let argument = quote type:
        Int
    let ty = meta.type_qualified(meta.call_site("consumer_support"), meta.ident("Wrapped"), [argument])
    let binding = meta.pattern_var(meta.ident("value"))
    let pattern = meta.pattern_qualified_ctor(meta.call_site("consumer_support"), meta.ident("Wrapped"), [binding])
    let item = meta.expr_name(meta.ident("item"))
    let body = quote expr:
        value + 1
    let matched = meta.expr_match(item, [meta.match_arm(pattern, body)])
    let supplied = meta.expr_raw(list.at(holes, 0))
    quote expr:
        (fn(item: ${ty}) -> Int: ${matched})(${supplied})

pub comptime fn dynamic_qualified(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let source = "definition_support.answer()"
    meta.expr_raw(source)

pub comptime fn dynamic_static_interpolation(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let sigil = "$"
    meta.expr_raw("\"" + sigil + "{not_in_scope}\"")
"#;

const CONSUMER: &str = r#"
import tag_library
import consumer_support

fn main(console: Console):
    console.print("${definition_qualified"ignored"}")
    console.print("${callsite_qualified"${consumer_support.Wrapped(41)}"}")
    console.print("${dynamic_qualified"ignored"}")
    console.print("${dynamic_static_interpolation"ignored"}")
"#;

#[test]
fn qualified_builder_origins_resolve_on_both_backends() {
    let modules = [
        ("definition_support", DEFINITION_SUPPORT),
        ("consumer_support", CONSUMER_SUPPORT),
        ("tag_library", TAG_LIBRARY),
        ("main", CONSUMER),
    ]
    .into_iter()
    .map(|(name, source)| {
        (
            name.to_string(),
            parser::parse_module(source)
                .unwrap_or_else(|error| panic!("parse {name}: {error}")),
        )
    })
    .collect();

    let linked = pipeline::link(modules, "main").expect("link qualified identity program");
    typeck::check(&linked).expect("typecheck qualified identity program");
    let rendered = format!("{linked:?}");
    assert!(!rendered.contains("@call_site"), "{rendered}");
    assert!(!rendered.contains("@definition_site"), "{rendered}");
    assert!(rendered.contains("definition_support.Wrapped"), "{rendered}");
    assert!(rendered.contains("consumer_support.Wrapped"), "{rendered}");

    let expected = vec![
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "${not_in_scope}".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    super::assert_compiled_output(&linked, &expected, "compile qualified identity program", 64);
}
