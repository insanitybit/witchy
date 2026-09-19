//! RFC-0080 compiler-owned impl and module builders.

use witchy::{interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

trait Answer:
    fn answer(self) -> Int

type Token:
    Token

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let answer = meta.type_named(meta.ident("Answer"), [])
    let token = meta.type_named(meta.ident("Token"), [])
    let receiver = meta.ident("self")
    let method = meta.function(false, meta.ident("answer"), [meta.param(receiver, token)], Some(int), meta.expr_int(42))
    emit_item(meta.impl_block(answer, token, [method]))

    let generated = meta.function(true, meta.ident("generated"), [], Some(int), meta.expr_int(7))
    let generated_module = meta.module([generated])
    for item in meta.module_items(generated_module):
        emit_item(item)

fn main(console: Console):
    console.print("${Token.answer()}")
    console.print("${generated()}")
"#;

#[test]
fn impl_and_module_builders_remain_structural_through_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural item builders");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural item builders");
    typeck::check(&linked).expect("typecheck structural item builders");

    let expected = vec!["42".to_string(), "7".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile", 128);
}

#[test]
fn generic_derive_eq_retains_the_type_parameter_bound() {
    let valid = r#"
import list

type Box(a) derive(PartialEq, Eq):
    value: a

fn main(console: Console):
    console.print("${list.length(list.unique([Box(1), Box(1)]))}")
"#;
    let parsed = parser::parse_module(valid).expect("parse valid generic Eq use");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link valid generic Eq use");
    typeck::check(&linked).expect("Int satisfies the derived Eq bound");

    let invalid = r#"
import list

type Box(a) derive(PartialEq, Eq):
    value: a

fn main(console: Console):
    let _ = list.unique([Box(1.0)])
"#;
    let parsed = parser::parse_module(invalid).expect("parse invalid generic Eq use");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand invalid generic Eq use");
    let error = typeck::check(&linked).expect_err("Float must not satisfy the derived Eq bound");
    assert!(error.to_string().contains("Eq"), "{error}");
}

#[test]
fn structural_item_import_wrapper_preserves_generated_module_imports() {
    let source = r#"
import meta

comptime:
    let json = meta.type_qualified(meta.ident("json"), meta.ident("Json"), [])
    let error = meta.type_qualified(meta.ident("json"), meta.ident("DeserializeError"), [])
    let result = meta.type_named(meta.ident("Result"), [meta.type_named(meta.ident("Int"), []), error])
    let input = meta.ident("input")
    let decoded = meta.expr_try(meta.expr_qualified_call(meta.ident("json"), meta.ident("int_of"), [meta.expr_name(input)]))
    let body = meta.expr_call(meta.expr_name(meta.ident("Ok")), [decoded])
    let generated = meta.function(true, meta.ident("generated"), [meta.param(input, json)], Some(result), body)
    emit_item(meta.item_with_imports(["json"], generated))

fn main(console: Console):
    match generated(json.JsonInt(7)):
        Ok(value) -> console.print("${value}")
        Err(_) -> console.print("error")
"#;
    let parsed = parser::parse_module(source).expect("parse structural item import wrapper");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link structural item import wrapper");
    typeck::check(&linked).expect("typecheck structural item import wrapper");

    let expected = vec!["7".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );
    super::assert_compiled_output(&linked, &["7".to_string()], "compile", 128);
}

#[test]
fn structural_item_import_wrapper_rejects_non_standard_imports() {
    let source = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let generated = meta.function(true, meta.ident("generated"), [], Some(int), meta.expr_int(7))
    emit_item(meta.item_with_imports(["not_a_standard_module"], generated))

fn main(console: Console):
    console.print("${generated()}")
"#;
    let parsed = parser::parse_module(source).expect("parse invalid structural import wrapper");
    let error = format!(
        "{:?}",
        pipeline::link(vec![("main".into(), parsed)], "main")
            .expect_err("non-standard generated imports must be rejected")
    );
    assert!(error.contains("standard-module"), "{error}");
}

#[test]
fn structural_item_import_wrapper_private_bridge_is_not_public() {
    let source = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let generated = meta.function(true, meta.ident("generated"), [], Some(int), meta.expr_int(7))
    let _ = meta.__meta_item_with_imports([], generated)
"#;
    let parsed = parser::parse_module(source).expect("parse private structural import bridge");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("private item import bridge must be rejected")
        .to_string();
    assert!(error.contains("compiler-private intrinsic"), "{error}");
}

#[test]
fn deserialize_generator_rejects_retired_raw_item_generation() {
    let source = include_str!("../../std/meta.witchy");
    let generator = source
        .split_once("pub fn derive_deserialize")
        .map(|(_, rest)| rest)
        .expect("derive(Deserialize) generator");
    assert!(!generator.contains("derive_deserialize_source"), "{generator}");
    assert!(!generator.contains("meta.item("), "{generator}");
    assert!(generator.contains("item_with_imports"), "{generator}");

    let decode_helpers = source
        .split_once("fn json_decode_value")
        .and_then(|(_, helpers)| helpers.split_once("pub fn derive_deserialize"))
        .map(|(helpers, _)| helpers)
        .expect("bounded structural decode helpers before derive(Deserialize)");
    assert!(decode_helpers.contains("expr_try"), "{decode_helpers}");
}

#[test]
fn module_syntax_is_compile_time_only() {
    let source = r#"
import meta

fn leak(value: meta.ModuleSyntax) -> meta.ModuleSyntax:
    value
"#;
    let parsed = parser::parse_module(source).expect("parse runtime ModuleSyntax leak");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link runtime ModuleSyntax leak");
    let error = typeck::check(&linked)
        .expect_err("ModuleSyntax must not cross into runtime code")
        .to_string();
    assert!(error.contains("meta.ModuleSyntax") && error.contains("compile-time-only"), "{error}");
}
