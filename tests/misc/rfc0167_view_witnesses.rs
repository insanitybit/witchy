//! Focused RFC-0167 rooted-view witnesses at the canonical execution boundary.
//!
//! These fixtures use an independent scalar expectation, the canonical SafeWIR
//! interpreter, and admitted unoptimized Wasm. They deliberately do not invoke
//! the source interpreter or optional optimizers.

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
    let error = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("normal".to_owned(), normal.as_bytes().to_vec()),
            ("app".to_owned(), opt_entry.as_bytes().to_vec()),
        ],
        "app",
        witchy_interp::comptime::expand_compile_time,
    )
    .expect_err("opt caller must reject an arbitrary normal dependency");
    let message = error.to_string();
    assert!(
        message.contains("not `mode opt`") || message.contains("optimization mode is transitive"),
        "the rejection must identify the transitive mode boundary: {message}",
    );
}
