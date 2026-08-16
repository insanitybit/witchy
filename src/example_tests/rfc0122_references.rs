use super::*;

#[test]
fn shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn first(text: &'a String) -> &'a String:
    text

fn main(console: Console):
    let text = "value"
    let observed = first(&text)
    console.print("${*observed}")
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves the shared returned place");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves the shared returned place");
}

#[test]
fn mutable_to_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn share(text: &'a mut String) -> &'a String:
    text

fn main(console: Console):
    var text = "value"
    let editable = &mut text
    let observed = share(editable)
    console.print("${*observed}")
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves the shared reborrow");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves the shared reborrow");
}
