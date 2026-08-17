use super::*;

#[test]
fn direct_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn first(text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var text = "value"
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

#[test]
fn shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n\nfn main(console: Console):\n    let text = \"value\"\n    let observed = first(&text)\n    console.print(\"${*observed}\")\n";
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves a directly borrowed shared place");
    let (compiled, _) = wasm_run_reowns(src);
    assert_eq!(compiled, want, "compiled backend preserves a directly borrowed shared place");
}

#[test]
fn shared_reference_tuple_preserves_each_owner_root_on_both_backends() {
    let src = r#"mode opt

fn pair(left: &'a String, right: &'b String) -> (&'a String, &'b String):
    (left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let tuple = pair(&first, &second)
    let copied = tuple
    let (left, right) = copied
    console.print(*left)
    console.print(*right)
    console.print(*tuple.0)
    console.print(*tuple.1)
"#;
    let want = ["first", "second", "first", "second"];
    assert_eq!(link_run(src), want, "interpreter preserves tuple reference owners");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves tuple reference owners");
}

#[test]
fn shared_reference_list_preserves_each_owner_root_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a String, right: &'a String) -> List(&'a String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&first, &second)
    console.print(*values[0])
    console.print(*values[1])
"#;
    let want = ["first", "second"];
    assert_eq!(link_run(src), want, "interpreter preserves list reference owners");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves list reference owners");
}
