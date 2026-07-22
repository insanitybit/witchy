use super::*;
use crate::{interpreter, parser, typeck};

    /// RFC-0080: tagged-literal generators can return typed `meta.ExprSyntax`
    /// instead of raw expression-source strings. The tag remains a `comptime fn`,
    /// so its compile-time-only return type never leaks into the runtime module.
    #[test]
    fn tagged_literals_can_return_typed_exprsyntax_on_both_backends() {
        let src = "from meta import ExprSyntax, expr_raw\nimport list\n\ncomptime fn add_one(parts: List(String), holes: List(String)) -> ExprSyntax:\n    expr_raw(\"(\" + list.at(holes, 0) + \" + 1)\")\n\nfn main(console: Console):\n    let n = 41\n    console.print(\"${add_one\"value ${n}\"}\")\n";
        let expected = ["42"];
        assert_eq!(link_run(src), expected, "interp typed tagged literal");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled typed tagged literal",
        );
    }

    /// RFC-0080 expression quotation: a typed tag returning a hole-free quote
    /// emits the compiler-owned expression AST directly. The anonymous record
    /// keeps the structural payload nontrivial while both runtime backends consume
    /// the same already-expanded tree.
    #[test]
    fn quote_expr_builds_typed_exprsyntax_on_both_backends() {
        let src = r#"
comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        .{value: 40}.value + 2

fn main(console: Console):
    console.print("${answer"ignored"}")
"#;
        let expected = ["42"];
        assert_eq!(link_run(src), expected, "interp quote expr tagged literal");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote expr tagged literal",
        );
    }

    /// RFC-0080 quote holes splice typed expression syntax into a parser-checked
    /// quoted expression. Tagged literals hand their holes to the tag as opaque
    /// markers, so this also exercises the RFC-0006 hygiene split.
    #[test]
    fn quote_expr_holes_splice_typed_exprsyntax_on_both_backends() {
        let src = r#"
import list
import meta

comptime fn add_one(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let value = meta.expr_raw(list.at(holes, 0))
    quote expr:
        ${value} + 1

fn main(console: Console):
    let n = 41
    console.print("${add_one"${n}"}")
"#;
        let expected = ["42"];
        assert_eq!(link_run(src), expected, "interp quote expr holes tagged literal");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote expr holes tagged literal",
        );
    }

    /// RFC-0080 type quotation retains compiler-owned type AST while existing
    /// builders consume its canonical projection without a public raw constructor.
    #[test]
    fn quote_type_builds_typed_typesyntax_on_both_backends() {
        let src = r#"
import meta

comptime:
    let int = quote type:
        Int
    emit_item(meta.function(true, meta.ident("generated"), [], Some(int), meta.expr_int(7)))

fn main(console: Console):
    console.print("${generated()}")
"#;
        let expected = ["7"];
        assert_eq!(link_run(src), expected, "interp quote type generated item");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote type generated item",
        );
    }

    /// RFC-0080 pattern quotation retains compiler-owned pattern AST while
    /// builders remain the compatibility path for composed match arms.
    #[test]
    fn quote_pattern_builds_typed_patternsyntax_on_both_backends() {
        let src = r#"
import meta

type Flag:
    Small
    Big(Int)

comptime:
    let flag = quote type:
        Flag
    let string = quote type:
        String
    let x = meta.ident("x")
    let value = meta.ident("value")
    let small_pat = quote pattern:
        Small
    let big_pat = quote pattern:
        Big(value)
    let small = meta.match_arm(small_pat, meta.expr_raw("\"small\""))
    let big = meta.match_arm(big_pat, meta.expr_raw("\"big:\" + \"$" + "{value}\""))
    emit_item(meta.function(true, meta.ident("classify"), [meta.param(x, flag)], Some(string), meta.expr_match(meta.expr_name(x), [small, big])))

fn main(console: Console):
    console.print(classify(Small))
    console.print(classify(Big(9)))
"#;
        let expected = ["small", "big:9"];
        assert_eq!(link_run(src), expected, "interp quote pattern generated match");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote pattern generated match",
        );
    }

    /// RFC-0080 type/pattern quote holes splice typed syntax values into
    /// parser-checked type and pattern quotations.
    #[test]
    fn quote_type_and_pattern_holes_splice_typed_syntax_on_both_backends() {
        let src = r#"
import meta
import option

comptime:
    let int = quote type:
        Int
    let maybe_int = quote type:
        Option(${int})
    let string = quote type:
        String
    let x = meta.ident("x")
    let value = meta.ident("value")
    let value_pat = quote pattern:
        value
    let some_pat = quote pattern:
        Some(${value_pat})
    let none_pat = quote pattern:
        None
    let some = meta.match_arm(some_pat, meta.expr_raw("\"value:\" + \"$" + "{value}\""))
    let none = meta.match_arm(none_pat, meta.expr_raw("\"none\""))
    emit_item(meta.function(true, meta.ident("describe"), [meta.param(x, maybe_int)], Some(string), meta.expr_match(meta.expr_name(x), [some, none])))

fn main(console: Console):
    console.print(describe(Some(5)))
    console.print(describe(None))
"#;
        let expected = ["value:5", "none"];
        assert_eq!(link_run(src), expected, "interp quote type/pattern holes");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote type/pattern holes",
        );
    }

    /// RFC-0080 statement/block quotation parses function-body fragments at the
    /// quote site and returns typed compiler syntax values for generation.
    #[test]
    fn quote_stmt_and_block_build_typed_body_syntax_on_both_backends() {
        let src = r#"
import meta

comptime:
    let int = quote type:
        Int
    let body = quote block:
        let x: Int = 40
        x + 2
    emit_item(meta.function_block(true, meta.ident("answer_block"), [], Some(int), body))

    let stmt = quote stmt:
        let y: Int = 5
    let tail = quote expr:
        y + 1
    let body2 = meta.block([stmt], Some(tail))
    emit_item(meta.function_block(true, meta.ident("answer_stmt"), [], Some(int), body2))

fn main(console: Console):
    console.print("${answer_block()}")
    console.print("${answer_stmt()}")
"#;
        let expected = ["42", "6"];
        assert_eq!(link_run(src), expected, "interp quote stmt/block");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote stmt/block",
        );
    }

    /// RFC-0080 statement/block quote holes splice typed expression syntax into
    /// parser-checked generated body fragments.
    #[test]
    fn quote_stmt_and_block_holes_splice_typed_exprsyntax_on_both_backends() {
        let src = r#"
import meta

comptime:
    let int = quote type:
        Int
    let forty = quote expr:
        40
    let two = quote expr:
        2
    let body = quote block:
        let x = ${forty}
        x + ${two}
    emit_item(meta.function_block(true, meta.ident("answer_block"), [], Some(int), body))

    let five = quote expr:
        5
    let stmt = quote stmt:
        let y = ${five}
    let one = quote expr:
        1
    let tail = quote expr:
        y + ${one}
    let body2 = meta.block([stmt], Some(tail))
    emit_item(meta.function_block(true, meta.ident("answer_stmt"), [], Some(int), body2))

fn main(console: Console):
    console.print("${answer_block()}")
    console.print("${answer_stmt()}")
"#;
        let expected = ["42", "6"];
        assert_eq!(link_run(src), expected, "interp quote stmt/block holes");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote stmt/block holes",
        );
    }

    /// RFC-0080 statement/block quote holes preserve the typed category of every
    /// splice, so body generators can combine type, pattern, and expression syntax
    /// without returning to string templates.
    #[test]
    fn quote_stmt_and_block_mixed_holes_splice_typed_syntax_on_both_backends() {
        let src = r#"
import meta

comptime:
    let int = quote type:
        Int
    let forty = quote expr:
        40
    let two = quote expr:
        2
    let bound = quote pattern:
        z
    let body = quote block:
        let x: ${int} = ${forty}
        let ${bound} = x + ${two}
        z
    emit_item(meta.function_block(true, meta.ident("answer_block"), [], Some(int), body))

    let six = quote expr:
        6
    let stmt = quote stmt:
        let y: ${int} = ${six}
    let tail = quote expr:
        y
    let body2 = meta.block([stmt], Some(tail))
    emit_item(meta.function_block(true, meta.ident("answer_stmt"), [], Some(int), body2))

fn main(console: Console):
    console.print("${answer_block()}")
    console.print("${answer_stmt()}")
"#;
        let expected = ["42", "6"];
        assert_eq!(link_run(src), expected, "interp quote stmt/block mixed holes");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote stmt/block mixed holes",
        );
    }

    /// RFC-0080 item quotation parses one item at the quote site and hands it to
    /// the existing typed `meta.item` boundary.
    #[test]
    fn quote_item_builds_typed_itemsyntax_on_both_backends() {
        let src = r#"
comptime:
    let generated = quote item:
        pub fn generated() -> Int:
            88
    emit_item(generated)

fn main(console: Console):
    console.print("${generated()}")
"#;
        let expected = ["88"];
        assert_eq!(link_run(src), expected, "interp quote item generated function");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote item generated function",
        );
    }

    /// RFC-0080 item quote holes keep the item boundary typed while splicing
    /// expression, type, and pattern syntax into the generated declaration.
    #[test]
    fn quote_item_mixed_holes_generate_typed_function_on_both_backends() {
        let src = r#"
comptime:
    let int = quote type:
        Int
    let forty = quote expr:
        40
    let bound = quote pattern:
        z
    let two = quote expr:
        2
    let generated = quote item:
        pub fn answer(x: ${int}) -> ${int}:
            let ${bound} = ${forty}
            z + x + ${two}
    emit_item(generated)

fn main(console: Console):
    console.print("${answer(0)}")
"#;
        let expected = ["42"];
        assert_eq!(link_run(src), expected, "interp quote item mixed holes");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled quote item mixed holes",
        );
    }

    /// (BUG-182) A tagged literal in a standalone file whose stem is NOT a valid
    /// identifier (`tag-hyphen`) must still expand and run on both backends. Tag
    /// expansion seeds a throwaway parse with `import <qualifier>` lines built from
    /// module names — including the CURRENT module's, which for a standalone file is
    /// its filesystem stem. A hyphenated stem produced an invalid `import tag-hyphen`
    /// line that broke every tag in such a file; non-identifier qualifiers are now
    /// skipped (they can never be referenced as `q.f(…)` anyway).
    #[test]
    fn tagged_literal_in_hyphenated_module_expands() {
        let src = "import meta\n\ncomptime fn lit(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_raw(\"\\\"ok\\\"\")\n\nfn main(console: Console):\n    console.print(lit\"ignored\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("tag-hyphen".into(), module)], "tag-hyphen")
            .expect("a tagged literal in a hyphenated-stem module must link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(interpreter::run_module(linked, ".", Vec::new()).expect("run"), ["ok"], "interp");
        assert_eq!(run_linked_on_wasm(&[("tag-hyphen", src)], "tag-hyphen"), ["ok"], "wasm");
    }

    /// (BUG-338) An escaped `\${...}` inside a tagged literal is static text, not
    /// a hole and not a second chance to capture a call-site binding when the tag's
    /// generated source is parsed. This pins both a minimal source-emitting tag and
    /// the flagship glamour `html` tag.
    #[test]
    fn tagged_literal_escaped_dollar_stays_literal_on_both_backends() {
        let plain = r#"import meta

comptime fn lit(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_raw("\"" + list.at(parts, 0) + "\"")

fn main(console: Console):
    let price = "CAPTURED"
    console.print(lit"cost \${price}")
"#;
        let expected_plain = ["cost ${price}"];
        assert_eq!(link_run(plain), expected_plain, "interp plain tag");
        assert_eq!(run_linked_on_wasm(&[("main", plain)], "main"), expected_plain, "wasm plain tag");

        let glamour = r#"import glamour
from glamour import VNode

type Msg:
    Click

fn main(console: Console):
    let price = "CAPTURED"
    let node: VNode(Msg) = html"<p>literal: \${price}</p>"
    console.print(glamour.to_html(node))
"#;
        let expected_glamour = ["<p>literal: ${price}</p>"];
        let glamour_src = include_str!("../../projects/glamour/src/glamour.witchy");
        let linked = crate::pipeline::link(
            vec![
                ("glamour".into(), parser::parse_module(glamour_src).expect("glamour parse")),
                ("main".into(), parser::parse_module(glamour).expect("main parse")),
            ],
            "main",
        )
        .expect("link glamour");
        typeck::check(&linked).expect("typecheck glamour");
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("run glamour"),
            expected_glamour,
            "interp glamour html"
        );
        assert_eq!(
            run_linked_on_wasm(&[("glamour", glamour_src), ("main", glamour)], "main"),
            expected_glamour,
            "wasm glamour html"
        );
    }
