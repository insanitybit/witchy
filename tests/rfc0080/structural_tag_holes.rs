//! RFC-0167 structural tagged-literal hole construction.

use witchy::{interpreter, parser, pipeline, typeck};

fn glamour() -> witchy::ast::Module {
    parser::parse_module(include_str!("../../projects/glamour/src/glamour.witchy"))
        .expect("parse Glamour structural tag fixture")
}

#[test]
fn structural_tag_holes_expand_css_html_and_jsx_without_raw_source() {
    let entry = parser::parse_module(
        r#"
import glamour
from glamour import VNode

fn main(console: Console):
    let name = "Ada"
    let html_node: VNode(Int) = html"<p>${name}</p>"
    let jsx_node: VNode(Int) = jsx"<p>${name}</p>"
    console.print(glamour.to_html(html_node))
    console.print(glamour.to_html(jsx_node))
    match glamour.css_px(12):
        Err(_) -> console.print("invalid")
        Ok(width) -> console.print(glamour.css_text(css".card { width: ${width}; }"))
"#,
    )
    .expect("parse CSS/HTML/JSX structural fixture");
    let linked = pipeline::link(
        vec![("main".into(), entry), ("glamour".into(), glamour())],
        "main",
    )
    .expect("structural CSS/HTML/JSX expansion");
    typeck::check(&linked).expect("typecheck structural CSS/HTML/JSX expansion");

    let debug = format!("{linked:?}");
    assert!(!debug.contains("TaggedLit"), "tags must be fully expanded: {debug}");
    assert!(!debug.contains("__witchy_hole_"), "active sentinels must be substituted: {debug}");
    let output = interpreter::run_module(linked, ".", Vec::new())
        .expect("interpret structural tags");
    assert_eq!(&output[..2], ["<p>Ada</p>", "<p>Ada</p>"]);
    assert!(
        output[2].contains("width: 12px"),
        "typed CSS hole was not structurally rendered: {output:?}"
    );
}

#[test]
fn structural_named_call_builder_emits_a_direct_call() {
    let module = parser::parse_module(
        r#"
import meta

comptime fn call_identity(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_named_call(meta.ident("identity"), [meta.expr_tag_hole(holes.at(0))])

fn identity(value: Int) -> Int:
    value

fn main(console: Console):
    console.print("${call_identity"${7}"}")
"#,
    )
    .expect("parse direct named-call fixture");
    let linked = pipeline::link(vec![("main".into(), module)], "main")
        .expect("expand direct named-call fixture");
    typeck::check(&linked).expect("typecheck direct named-call fixture");
    let debug = format!("{linked:?}");
    assert!(debug.contains("Call { name: \"main.identity\""), "expected direct call: {debug}");
    assert!(
        !debug.contains("Apply { func: Var(\"identity\")"),
        "named call must not become an indirect apply: {debug}"
    );
    assert_eq!(
        interpreter::run_module(linked, ".", Vec::new()).expect("interpret direct named-call fixture"),
        vec!["7".to_string()]
    );
}

#[test]
fn structural_tag_hole_accepts_only_an_active_compiler_sentinel() {
    let valid = parser::parse_module(
        r#"
import meta

comptime fn duplicate(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let hole = meta.expr_tag_hole(holes.at(0))
    meta.expr_binary("+", hole, hole)

fn main(console: Console):
    console.print("${duplicate"${21}"}")
"#,
    )
    .expect("parse structural tag-hole fixture");
    let linked = pipeline::link(vec![("main".into(), valid)], "main")
        .expect("active sentinel is accepted");
    typeck::check(&linked).expect("typecheck structural tag-hole fixture");
    assert_eq!(
        interpreter::run_module(linked, ".", Vec::new()).expect("interpret structural tag-hole fixture"),
        vec!["42".to_string()],
    );

    for marker in ["not_a_hole", "__witchy_hole_99"] {
        let source = format!(
            "import meta\n\ncomptime fn forged(parts: List(String), holes: List(String)) -> meta.ExprSyntax:\n    meta.expr_tag_hole(\"{marker}\")\n\nfn main() -> Int:\n    forged\"x${{1}}\"\n"
        );
        let parsed = parser::parse_module(&source).expect("parse forged tag-hole fixture");
        let error = pipeline::link(vec![("main".into(), parsed)], "main")
            .expect_err("arbitrary or inactive sentinels must be rejected")
            .to_string();
        assert!(
            error.contains("compiler-issued hole marker for the active tagged literal"),
            "{error}"
        );
    }
}

#[test]
fn structural_tagged_literals_require_owned_holes_and_valid_shape() {
    let mismatched = parser::parse_module(
        r#"
import meta

comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_tagged(meta.ident("inner"), ["", ""], [])

fn main() -> Int:
    outer"value"
"#,
    )
    .expect("parse mismatched structural tag fixture");
    let error = pipeline::link(vec![("main".into(), mismatched)], "main")
        .expect_err("tagged parts and holes must agree")
        .to_string();
    assert!(
        error.contains("parts.len() == holes.len() + 1"),
        "{error}"
    );

    let forged = parser::parse_module(
        r#"
import meta

comptime fn outer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_tagged(meta.ident("inner"), ["", ""], [meta.ExprSyntax("42")])

fn main() -> Int:
    outer"value"
"#,
    )
    .expect("parse forged ExprSyntax fixture");
    let error = pipeline::link(vec![("main".into(), forged)], "main")
        .expect_err("source-backed ExprSyntax must be unforgeable")
        .to_string();
    assert!(
        error.contains("sealed") && error.contains("ExprSyntax"),
        "{error}"
    );

    let runtime = parser::parse_module(
        r#"
import meta

fn main():
    let _ = meta.expr_tagged(meta.ident("inner"), [""], [])
"#,
    )
    .expect("parse runtime misuse fixture");
    let linked = pipeline::link(vec![("main".into(), runtime)], "main")
        .expect("runtime misuse links before execution");
    let error = typeck::check(&linked)
        .expect_err("sealed Ident makes runtime structural-tag construction unreachable")
        .to_string();
    assert!(
        error.contains("call to `meta.ident`") && error.contains("compile-time-only"),
        "{error}"
    );
}
