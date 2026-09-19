//! RFC-0167 typed collection, tuple, and named-record expression builders.

use witchy::{ast::{BinOp, Expr, Item, Stmt}, comptime, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import list
import meta

type Pair:
    first: Int
    second: String

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let list_int = meta.type_named(meta.ident("List"), [int])
    let tuple_type = meta.type_tuple([meta.type_named(meta.ident("String"), []), int])
    let pair_type = meta.type_named(meta.ident("Pair"), [])
    let values = meta.expr_list([
        meta.expr_int(7),
        meta.expr_binary("+", meta.expr_int(40), meta.expr_int(2)),
    ])
    let tuple = meta.expr_tuple([
        meta.expr_string("\" + malicious() + \" \\ \${malicious}"),
        meta.expr_binary("+", meta.expr_int(40), meta.expr_int(2)),
    ])
    let pair = meta.expr_record(meta.ident("Pair"), [
        meta.record_field(meta.ident("first"), meta.expr_int(7)),
        meta.record_field(meta.ident("second"), meta.expr_string("\" + malicious() + \" \\ \${malicious}")),
    ])
    let updated = meta.expr_record_update(meta.ident("Pair"), [
        meta.record_field(meta.ident("first"), meta.expr_int(42)),
    ], pair)
    emit_item(meta.function(true, meta.ident("made_list"), [], Some(list_int), values))
    emit_item(meta.function(true, meta.ident("made_tuple"), [], Some(tuple_type), tuple))
    emit_item(meta.function(true, meta.ident("made_pair"), [], Some(pair_type), updated))

fn malicious() -> String:
    "executed"

fn main(console: Console):
    console.print("${list.at(made_list(), 1)}")
    let (tuple_text, tuple_number) = made_tuple()
    console.print(tuple_text)
    console.print("${tuple_number}")
    let pair = made_pair()
    console.print("${pair.first}")
    console.print(pair.second)
"#;

fn generated_expr<'a>(module: &'a witchy::ast::Module, name: &str) -> &'a Expr {
    let function = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name.rsplit('.').next() == Some(name) => Some(function),
        _ => None,
    }).unwrap_or_else(|| panic!("missing generated function `{name}`"));
    match function.body.stmts.last() {
        Some(Stmt::Expr(expr)) => expr,
        other => panic!("expected expression body for `{name}`, got {other:?}"),
    }
}

#[test]
fn collection_and_record_builders_transfer_owned_ast_without_source_injection() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural collection builders");
    let mut expanded = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded, &[])
        .expect("expand structural collection builders");

    assert!(matches!(
        generated_expr(&expanded, "made_list"),
        Expr::List(elements)
            if matches!(elements.as_slice(), [Expr::Int(7), Expr::Binary { op: BinOp::Add, lhs, rhs }]
                if matches!(lhs.as_ref(), Expr::Int(40)) && matches!(rhs.as_ref(), Expr::Int(2)))
    ));
    assert!(matches!(
        generated_expr(&expanded, "made_tuple"),
        Expr::Tuple(elements)
            if matches!(elements.as_slice(), [Expr::Str(value), Expr::Binary { op: BinOp::Add, lhs, rhs }]
                if value == "\" + malicious() + \" \\ ${malicious}"
                    && matches!(lhs.as_ref(), Expr::Int(40))
                    && matches!(rhs.as_ref(), Expr::Int(2)))
    ));
    assert!(matches!(
        generated_expr(&expanded, "made_pair"),
        Expr::Record { name, fields, spread: Some(base) }
            if name == "Pair"
                && matches!(fields.as_slice(), [(field, Expr::Int(42))] if field == "first")
                && matches!(base.as_ref(), Expr::Record { name, fields, spread: None }
                    if name == "Pair"
                        && matches!(fields.as_slice(), [
                            (first, Expr::Int(7)),
                            (second, Expr::Str(value)),
                        ] if first == "first" && second == "second"
                            && value == "\" + malicious() + \" \\ ${malicious}"))
    ));

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link structural collection builders");
    typeck::check(&linked).expect("typecheck structural collection builders");

    let expected = vec![
        "42".to_string(),
        "\" + malicious() + \" \\ ${malicious}".to_string(),
        "42".to_string(),
        "42".to_string(),
        "\" + malicious() + \" \\ ${malicious}".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );
    super::assert_compiled_output(&linked, &expected, "compile", 64);
}

#[test]
fn record_builder_rejects_duplicate_fields_and_call_site_names() {
    for (source, expected) in [
        (
            r#"
                import meta
                comptime:
                    let _ = meta.expr_record(meta.ident("Pair"), [
                        meta.record_field(meta.ident("value"), meta.expr_int(1)),
                        meta.record_field(meta.ident("value"), meta.expr_int(2)),
                    ])
            "#,
            "meta.expr_record: duplicate field `value`",
        ),
        (
            r#"
                import meta
                comptime:
                    let _ = meta.expr_record(meta.call_site("Pair"), [])
            "#,
            "meta.expr_record requires a definition-site identifier; meta.call_site is reference-only",
        ),
    ] {
        let parsed = parser::parse_module(source).expect("parse invalid record builder");
        let error = pipeline::link(vec![("main".into(), parsed)], "main")
            .expect_err("record builder must reject invalid structural inputs")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}
