//! RFC-0165 A1: keep the native SafeWIR security surface on the required path.

// A feature-disabled test target would otherwise compile successfully while
// silently removing every native execution test in canonical_wasmir.rs.
#[cfg(not(feature = "native"))]
compile_error!(
    "RFC-0165 native security tests require `cargo test/nextest ... --features native`"
);

#[test]
fn native_security_surface_is_compiled_and_selected() {
    assert!(cfg!(feature = "native"));
}
