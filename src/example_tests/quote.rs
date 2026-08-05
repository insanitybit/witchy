use super::*;
use crate::{interpreter, parser, typeck};

/// (BUG-182) A tagged literal in a standalone file whose stem is NOT a valid
/// identifier (`tag-hyphen`) must still expand and run on both backends.
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
/// a hole or a call-site capture. This pins both a minimal tag and glamour's
/// `html` tag.
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
