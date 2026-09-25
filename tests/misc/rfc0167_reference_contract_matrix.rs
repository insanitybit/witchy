//! RFC-0167 immutable rooted-view source contract.
//!
//! This matrix deliberately exercises the post-lifetime surface. It is a
//! checked-source boundary, not a second runtime oracle: canonical execution
//! coverage belongs to admitted rooted-view tests, while carrier forms that
//! are not yet admitted remain explicit checked-source cases here.

fn checks(label: &str, source: &str) {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} must pass checked-source admission: {error}"));
}

fn rejects(label: &str, source: &str, required: &[&str]) {
    let error = witchy::resolve_std_only_checked(source)
        .expect_err(label)
        .to_string();
    assert!(
        required.iter().any(|fragment| error.contains(fragment)),
        "{label} must fail closed with one of {required:?}: {error}",
    );
}

#[test]
fn immutable_views_use_one_surface_in_normal_and_opt_modes() {
    for mode in ["", "mode opt\n"] {
        checks(
            "direct, projected, slice, generic, and var snapshot views",
            &format!(
                r#"{mode}

fn first[a](items: &[a]) -> Option(&a):
    Some(&items[0])

fn identity[a](value: &a) -> &a:
    value

fn main():
    var text = "before"
    let text_view = identity(&text)
    let item_view = &text
    let numbers = [1, 2, 3]
    let slice_view = &numbers[1..3]
    let first_view = first(slice_view)
    text = "after"
    let _ = text_view
    let _ = item_view
    let _ = first_view
"#,
            ),
        );
    }
}

#[test]
fn immutable_views_are_valid_aggregate_and_callable_payloads_without_lifetimes() {
    checks(
        "aggregate field and generic transport retain immutable view syntax",
        r#"mode opt

type Holder:
    view: &String

fn relay[a](value: &a) -> &a:
    value

fn make(owner: String) -> Holder:
    Holder(relay(&owner))

fn main():
    let holder = make("value")
    let _ = holder
"#,
    );

}

#[test]
fn var_is_the_only_mutation_and_write_back_surface() {
    checks(
        "var parameter write-back does not use a mutable reference",
        r#"fn increment(var value: Int):
    value = value + 1

fn main():
    var value = 1
    increment(value)
"#,
    );

    for (label, source) in [
        (
            "explicit lifetime",
            "fn bad(value: &'a String) -> &'a String:\n    value\n",
        ),
        (
            "mutable reference",
            "fn bad(value: &'a mut String):\n    value\n",
        ),
        (
            "mutable view expression",
            "fn bad():\n    var value = \"value\"\n    let view = &mut value\n    let _ = view\n",
        ),
    ] {
        rejects(label, source, &["lifetime", "&mut", "mutable", "reference", "removed"]);
    }
}
