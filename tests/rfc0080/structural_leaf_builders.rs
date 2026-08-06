//! RFC-0080 compiler-owned primitive expression, pattern, and statement builders.

use witchy::{interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

fn plus_one(value: Int) -> Int:
    value + 1

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let bool = meta.type_named(meta.ident("Bool"), [])
    let string = meta.type_named(meta.ident("String"), [])
    let duration = meta.type_named(meta.ident("Duration"), [])

    let call = meta.expr_call(meta.expr_name(meta.ident("plus_one")), [meta.expr_int(41)])
    emit_item(meta.function(true, meta.ident("generated_call"), [], Some(int), call))
    emit_item(meta.function(true, meta.ident("generated_bool"), [], Some(bool), meta.expr_bool(true)))

    let int_value = meta.ident("value")
    let int_match = meta.expr_match(meta.expr_name(int_value), [
        meta.match_arm(meta.pattern_range(1, 3, true), meta.expr_bool(true)),
        meta.match_arm(meta.pattern_wildcard(), meta.expr_bool(false)),
    ])
    emit_item(meta.function(true, meta.ident("int_pattern"), [meta.param(int_value, int)], Some(bool), int_match))

    let string_value = meta.ident("value")
    let string_match = meta.expr_match(meta.expr_name(string_value), [
        meta.match_arm(meta.pattern_str("witchy"), meta.expr_bool(true)),
        meta.match_arm(meta.pattern_var(meta.ident("other")), meta.expr_bool(false)),
    ])
    emit_item(meta.function(true, meta.ident("string_pattern"), [meta.param(string_value, string)], Some(bool), string_match))

    let duration_value = meta.ident("value")
    let duration_match = meta.expr_match(meta.expr_name(duration_value), [
        meta.match_arm(meta.pattern_duration_ms(1000), meta.expr_bool(true)),
        meta.match_arm(meta.pattern_wildcard(), meta.expr_bool(false)),
    ])
    emit_item(meta.function(true, meta.ident("duration_pattern"), [meta.param(duration_value, duration)], Some(bool), duration_match))

    let empty_return = meta.block([meta.stmt_return_none()], None)
    emit_item(meta.function_block(true, meta.ident("generated_return"), [], None, empty_return))

fn main(console: Console):
    console.print("${generated_call()}")
    console.print("${generated_bool()}")
    console.print("${int_pattern(2)}")
    console.print("${int_pattern(9)}")
    console.print("${string_pattern(\"witchy\")}")
    console.print("${duration_pattern(1s)}")
    generated_return()
"#;

#[test]
fn primitive_builders_remain_structural_through_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural leaf builders");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural leaf builders");
    typeck::check(&linked).expect("typecheck structural leaf builders");

    let expected = vec![
        "42".to_string(),
        "true".to_string(),
        "true".to_string(),
        "false".to_string(),
        "true".to_string(),
        "true".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile", 128);
}

#[test]
fn pattern_binding_rejects_call_site_identifiers() {
    let source = r#"
import meta

comptime:
    let _ = meta.pattern_var(meta.call_site("value"))
"#;
    let parsed = parser::parse_module(source).expect("parse invalid pattern binding");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("call-site references are not binders")
        .to_string();
    assert!(error.contains("meta.pattern_var") && error.contains("reference-only"), "{error}");
}
