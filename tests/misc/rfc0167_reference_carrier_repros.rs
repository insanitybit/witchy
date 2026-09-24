//! RFC-0167 immutable generic-view carrier boundaries.
//!
//! Historical versions of this file exercised mutable loans and used the
//! source tree walker as an oracle. Those rows are retired. The surviving
//! rows use only `&T`/`&[T]` and record the current canonical admission
//! boundary without claiming that checked source alone proves execution.

use witchy::codegen;

fn checked(label: &str, source: &str) {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} must pass checked-source admission: {error}"));
}

fn canonical_refusal(label: &str, source: &str) {
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} must pass checked-source admission: {error}"));
    let refusal = codegen::compile_checked_module_artifact(&checked).expect_unsupported(label);
    assert!(
        refusal.message.contains("unsupported")
            || refusal.message.contains("canonical")
            || refusal.message.contains("view"),
        "{label} must fail closed at canonical lowering: {refusal}",
    );
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
fn ambiguous_generic_view_projection_is_checked_but_not_lowered_by_erasure() {
    canonical_refusal(
        "generic shared relay with two possible roots",
        r#"mode opt

fn relay[a](left: &a, right: &a, choose_left: Bool) -> &a:
    if choose_left:
        left
    else:
        right

fn main():
    // A whole `&List(List(Int))` has an authenticated carrier. A third List
    // shell does not, so choosing between two such roots must still refuse.
    let left = [[[1]]]
    let right = [[[2]]]
    let _ = relay(&left, &right, true)
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
    let admitted = witchy::resolve_std_only_checked(
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
    let _ = codegen::compile_checked_module_artifact(&admitted)
        .expect_lowered("callable immutable view carrier");

    // A view payload with no carrier stays fail-closed inside a callable.
    canonical_refusal(
        "callable view without a carrier",
        r#"mode opt

fn apply(action: fn(&List(List(List(Int)))) -> Int, value: &List(List(List(Int)))) -> Int:
    action(value)

fn inspect(value: &List(List(List(Int)))) -> Int:
    (*value).length()

fn main():
    let rows = [[[1]]]
    let _ = apply(inspect, &rows)
"#,
    );
}
