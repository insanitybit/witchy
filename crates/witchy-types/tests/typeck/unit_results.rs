//! RFC-0167 row C: unit syntax is one source representation and one ABI shape.

use super::source::checked;

#[test]
fn public_inferred_nonunit_free_functions_and_methods_are_rejected_at_the_source_boundary() {
    for source in [
        "pub fn value():\n    1\n\nfn main():\n    ()\n",
        "type Meter:\n    Meter(Int)\n\nimpl Meter:\n    pub fn value(self):\n        1\n\nfn main():\n    ()\n",
    ] {
        let error = match checked(source) {
            Ok(_) => panic!("a public inferred non-unit result must not become a source ABI"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("inferred non-unit"),
            "source-boundary diagnostic should identify the missing public result annotation: {error}",
        );
    }
}
