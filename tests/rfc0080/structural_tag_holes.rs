//! RFC-0167 structural tagged-literal hole construction.

use witchy::{interpreter, parser, pipeline, typeck};
use witchy::opt::{self, OptSet};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn typed_tag_hole_reaches_admitted_wasm_without_reparse() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // Hand calculation: the same parser-owned 21 is substituted twice; 21 + 21 = 42.
    let source = r#"
import meta

comptime fn duplicate(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    let hole = meta.expr_tag_hole_typed(holes.at(0))
    meta.expr_binary("+", hole, hole)

fn main(console: Console):
    console.print("${duplicate"${21}"}")
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("typed compiler-issued tag hole type-checks");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |_, _| (),
    )
    .expect_lowered("typed tag hole admits through canonical SafeWIR");
    let mut runtime = witchy::runtime::Runtime::batch().expect("compiled runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..witchy::runtime::Capabilities::none()
            },
            64,
        )
        .expect("admitted unoptimized Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["42".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn typed_tag_hole_rejects_forged_and_wrongly_typed_holes() {
    for (source, expected) in [
        (
            r#"
import meta
comptime fn forged(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    meta.expr_tag_hole_typed(meta.TagHole("__witchy_hole_0"))
fn main() -> Int:
    forged"x${1}"
"#,
            "sealed",
        ),
        (
            r#"
import meta
comptime fn borrowed(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_tag_hole_typed(meta.TagHole(holes.at(0)))
fn main() -> Int:
    borrowed"x${1}"
"#,
            "sealed",
        ),
        (
            r#"
import meta
comptime fn wrong(parts: List(String), holes: List(Int)) -> meta.ExprSyntax:
    meta.expr_int(42)
fn main() -> Int:
    wrong"x${1}"
"#,
            "must accept holes as List(meta.TagHole) or legacy List(String)",
        ),
    ] {
        let parsed = parser::parse_module(source).expect("parse typed tag-hole near miss");
        let error = pipeline::link(vec![("main".into(), parsed)], "main")
            .expect_err("unsupported tag hole form must fail closed")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn parser_owned_tag_hole_reaches_admitted_wasm_without_reparse() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // Hand calculation: base = 40, identity(40 + 2) = 42. The tag places the
    // parser-owned expression once and the program prints its decimal value.
    let source = r#"
import meta

comptime fn splice(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_tag_hole(holes.at(0))

fn identity(value: Int) -> Int:
    value

fn main(console: Console):
    let base = 40
    console.print("${splice"${identity(base + 2)}"}")
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("parser-owned tagged hole type-checks");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |_, _| (),
    )
    .expect_lowered("parser-owned tagged hole admits through canonical SafeWIR");
    let mut runtime = witchy::runtime::Runtime::batch().expect("compiled runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..witchy::runtime::Capabilities::none()
            },
            64,
        )
        .expect("admitted unoptimized Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["42".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

fn glamour() -> witchy::ast::Module {
    parser::parse_module(include_str!("../../projects/glamour/src/glamour.witchy"))
        .expect("parse Glamour structural tag fixture")
}

#[test]
fn imported_generic_vnode_renderer_admits_exact_checked_instance() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // Hand calculation: the static p element contains one text child, Ada.
    // HTML serialization therefore produces exactly <p>Ada</p>.
    let source = r#"
import glamour
from glamour import VNode

fn main(console: Console):
    let node: VNode(Int) = html"<p>Ada</p>"
    console.print(glamour.to_html(node))
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("imported generic renderer type-checks from authenticated source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("checked VNode(Int) renderer admits to canonical SafeWIR");
    let mut runtime = witchy::runtime::Runtime::batch().expect("compiled runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..witchy::runtime::Capabilities::none()
            },
            64,
        )
        .expect("admitted unoptimized Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["<p>Ada</p>".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
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
