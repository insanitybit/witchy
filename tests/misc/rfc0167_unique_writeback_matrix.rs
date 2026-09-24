//! RFC-0167 row E: `var unique` write-back never duplicates its root owner.
//!
//! A `var unique` parameter may mutate and write back its original root while
//! returning a *different*, freshly established unique root.  Returning that
//! parameter itself would create two owning outputs for one root and must be
//! rejected before either interpreter or Wasm lowering.

fn check(source: &str) -> Result<witchy_types::pipeline::CheckedModule, witchy::ResolveStdError> {
    witchy::resolve_std_only_checked(source)
}

fn assert_rejects_duplicate_unique_writeback(source: &str) {
    let error = match check(source) {
        Ok(_) => panic!("a unique write-back root cannot also be returned as a unique result"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("root-owner proof") || error.contains("unique") || error.contains("write-back"),
        "duplicate unique/write-back rejection must remain source-visible: {error}"
    );
}

#[test]
fn direct_unique_writeback_cannot_return_its_parameter_in_normal_or_opt_mode() {
    let body = r#"
fn duplicate(var values: unique List(Int)) -> unique List(Int):
    values
"#;
    for source in [body.to_owned(), format!("mode opt\n{body}")] {
        assert_rejects_duplicate_unique_writeback(&source);
    }
}

