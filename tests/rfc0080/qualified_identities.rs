//! RFC-0080 compiler-owned identities for module-qualified type and pattern builders.

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

pub comptime fn definition_qualified(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
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

pub comptime fn callsite_qualified(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    let argument = quote type:
        Int
    let ty = meta.type_qualified(meta.call_site("consumer_support"), meta.ident("Wrapped"), [argument])
    let binding = meta.pattern_var(meta.ident("value"))
    let pattern = meta.pattern_qualified_ctor(meta.call_site("consumer_support"), meta.ident("Wrapped"), [binding])
    let item = meta.expr_name(meta.ident("item"))
    let body = quote expr:
        value + 1
    let matched = meta.expr_match(item, [meta.match_arm(pattern, body)])
    let supplied = meta.expr_tag_hole_typed(holes.at(0))
    quote expr:
        (fn(item: ${ty}) -> Int: ${matched})(${supplied})

pub comptime fn dynamic_qualified(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    meta.expr_qualified_call(meta.ident("definition_support"), meta.ident("answer"), [])

comptime fn literal_passthrough(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    meta.expr_tag_hole_typed(holes.at(0))

pub comptime fn dynamic_static_interpolation(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    let literal = meta.expr_string("\${not_in_scope}")
    meta.expr_tagged(meta.ident("literal_passthrough"), ["", ""], [literal])
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
    let expected = vec![
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "${not_in_scope}".to_string(),
    ];
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        [
            ("definition_support".into(), DEFINITION_SUPPORT.as_bytes().to_vec()),
            ("consumer_support".into(), CONSUMER_SUPPORT.as_bytes().to_vec()),
            ("tag_library".into(), TAG_LIBRARY.as_bytes().to_vec()),
            ("main".into(), CONSUMER.as_bytes().to_vec()),
        ].into(),
        "main",
        witchy_interp::comptime::EXPANDER,
    ).expect("authenticate and check qualified builder program");
    let rendered = format!("{:?}", checked.module());
    assert!(!rendered.contains("@call_site"), "{rendered}");
    assert!(!rendered.contains("@definition_site"), "{rendered}");
    assert!(rendered.contains("definition_support.Wrapped"), "{rendered}");
    assert!(rendered.contains("consumer_support.Wrapped"), "{rendered}");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit qualified builder program");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true,
            quiet: true,
            ..witchy::runtime::Capabilities::none()
        },
        64,
    ).expect("spawn admitted qualified builder program");
    actor.run().expect("run admitted qualified builder program");
    assert_eq!(actor.output(), expected);
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn ordinary_qualified_type_still_requires_import() {
    let error = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("definition_support".into(), DEFINITION_SUPPORT.as_bytes().to_vec()),
            ("main".into(), b"fn main(value: definition_support.Wrapped(Int)) -> Int:\n    0\n".to_vec()),
        ],
        "main",
        witchy_interp::comptime::EXPANDER,
    ).expect_err("an ordinary qualified type has no definition-site authority");
    assert!(error.to_string().contains("module `definition_support` is not imported"), "{error}");
}
