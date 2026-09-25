use super::*;

/// (BUG-338) An escaped `\${...}` inside a tagged literal is static text, not
/// a hole or a call-site capture.
#[test]
fn tagged_literal_escaped_dollar_stays_literal_on_both_backends() {
    if !is_nightly() {
        return;
    }
    let plain = r#"import meta

comptime fn lit(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    meta.expr_string(list.at(parts, 0))

fn main(console: Console):
    let price = "CAPTURED"
    console.print(lit"cost \${price}")
"#;
    let expected_plain = ["cost ${price}"];
    assert_eq!(link_run(plain), expected_plain, "interp plain tag");
    assert_eq!(run_linked_on_wasm(&[("main", plain)], "main"), expected_plain, "wasm plain tag");
}
