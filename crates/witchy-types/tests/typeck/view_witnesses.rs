//! Focused RFC-0167 rooted-view rejection at the checked-source boundary.

#[test]
fn opt_import_of_arbitrary_normal_module_rejects_at_transitive_mode_boundary() {
    let normal = r#"
pub fn relay(view: &Int) -> &Int:
    view
"#;
    let opt_entry = r#"
mode opt

import normal

fn main() -> Int:
    var value = 7
    let observed = normal.relay(&value)
    value = 9
    (*observed) * 10 + value
"#;
    let error = super::source::checked_bundle(
        vec![("normal", normal), ("app", opt_entry)],
        "app",
    )
    .expect_err("opt caller must reject an arbitrary normal dependency");
    let message = error.to_string();
    assert!(
        message.contains("not `mode opt`") || message.contains("optimization mode is transitive"),
        "the rejection must identify the transitive mode boundary: {message}",
    );
}
