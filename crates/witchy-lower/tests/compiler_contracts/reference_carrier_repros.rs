//! RFC-0167 immutable generic-view carrier boundaries.
//!
//! Historical versions of this file exercised mutable loans and used the
//! source tree walker as an oracle. Those rows are retired. The surviving
//! rows use only `&T`/`&[T]` and record the current canonical admission
//! boundary without claiming that checked source alone proves execution.

use super::try_checked;
use witchy_lower::codegen::compile_checked_module_artifact;

fn checked(label: &str, source: &str) {
    try_checked(source)
        .unwrap_or_else(|error| panic!("{label} must pass checked-source admission: {error}"));
}

#[test]
fn generic_immutable_views_keep_their_payload_type_and_projection() {
    checked(
        "generic immutable view relay",
        r#"mode opt

fn relay[a](value: &a) -> &a:
    value

fn first[a](items: &[a]) -> Option(&a):
    Some(&items[0])

fn main():
    let text = "before"
    let values = [3, 5]
    let text_view = relay(&text)
    let values_view = &values[0..2]
    let first_view = first(values_view)
    let _ = text_view
    let _ = first_view
"#,
    );
}

#[test]
fn immutable_view_declarations_do_not_alias_owned_signatures() {
    checked(
        "shared declaration before owned peer",
        r#"mode opt

fn view(value: &String) -> &String:
    value

fn owned(value: String) -> String:
    value

fn main():
    let text = "view"
    let _ = view(&text)
    let _ = owned("owned")
"#,
    );
    checked(
        "owned declaration before shared peer",
        r#"mode opt

fn owned(value: String) -> String:
    value

fn view(value: &String) -> &String:
    value

fn main():
    let text = "view"
    let _ = view(&text)
    let _ = owned("owned")
"#,
    );
}

#[test]
fn aggregate_and_callable_view_carriers_have_explicit_current_boundaries() {
    checked(
        "aggregate field retains immutable view syntax",
        r#"mode opt

type Holder:
    view: &String

fn make(owner: String) -> Holder:
    Holder(&owner)
"#,
    );

    // A callable whose signature carries a direct view passes the view's own
    // retained-root carrier through the indirect call, exactly as a direct
    // call does (`rfc0167_async_view_results` runs one).
    let admitted = try_checked(
        r#"mode opt

fn apply(action: fn(&String) -> String, value: &String) -> String:
    action(value)

fn inspect(value: &String) -> String:
    *value

fn main():
    let text = "value"
    let _ = apply(inspect, &text)
"#,
    )
    .expect("callable immutable view carrier must pass checked-source admission");
    let _ = compile_checked_module_artifact(&admitted)
        .expect_lowered("callable immutable view carrier");

    let qualified = try_checked(
        r#"mode opt

fn apply(action: fn(own &frozen Int) -> &frozen Int, own value: &frozen Int) -> Int:
    let result = action(value)
    *result

fn relay(own value: &frozen Int) -> &frozen Int:
    value

fn main() -> Int:
    let n: frozen Int = 11
    apply(relay, &n)
"#,
    )
    .expect("qualified scalar-place callable passes checked-source admission");
    let _ = compile_checked_module_artifact(&qualified)
        .expect_lowered("qualified scalar-place callable");

    let captured = try_checked(
        r#"mode opt

fn main() -> Int:
    let n: frozen Int = 11
    let view = &n
    let alias = view
    let read = fn() -> Int: *alias
    read()
"#,
    )
    .expect("inferred immutable capture retains its checked qualified view type");
    let _ = compile_checked_module_artifact(&captured)
        .expect_lowered("inferred qualified scalar-place capture");

    let patterned = try_checked(
        r#"mode opt

fn main() -> Int:
    let n: frozen Int = 11
    let view = &n
    let (alias,) = (view,)
    let read = fn() -> Int: *alias
    read()
"#,
    )
    .expect("tuple pattern preserves the inferred qualified view source");
    let _ = compile_checked_module_artifact(&patterned)
        .expect_lowered("tuple-pattern qualified scalar-place capture");

    let ordinary_call = try_checked(
        r#"mode opt

fn make(own value: Int) -> &Int:
    &value

fn main() -> Int:
    let view = make(11)
    let read = fn() -> Int: *view
    read()
"#,
    )
    .expect("ordinary call result view passes source checking");
    let _ = compile_checked_module_artifact(&ordinary_call)
        .expect_lowered("ordinary call result view capture");

    let unproved_field = try_checked(
        r#"mode opt

type Box:
    Box(&frozen Int)

fn main() -> Int:
    var n = 11
    let box = Box(&n)
    let Box(alias) = box
    let read = fn() -> Int: *alias
    read()
"#,
    )
    .expect("constructor field qualifier is erased by source checking");
    let unproved_field = compile_checked_module_artifact(&unproved_field)
        .expect_unsupported("constructor pattern cannot mint a qualified capture");
    assert!(unproved_field.to_string().contains("stable checked storage qualifier"), "{unproved_field}");

    let unstable = try_checked(
        r#"mode opt

fn main() -> Int:
    let n: frozen Int = 11
    var view = &n
    let read = fn() -> Int: *view
    read()
"#,
    )
    .expect("mutable view declaration passes the source checker");
    let unstable = compile_checked_module_artifact(&unstable)
        .expect_unsupported("mutable inferred view qualifier has no stable capture proof");
    assert!(unstable.to_string().contains("stable checked storage qualifier"), "{unstable}");

}
