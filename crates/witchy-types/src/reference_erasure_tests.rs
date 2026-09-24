// These fixtures exercise the retired lifetime-bearing migration grammar.
use super::check_reference_migration_str as check_str;

#[test]
fn canonical_reference_nominal_cannot_erase_into_owned_existential() {
    let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\ntrait Mark:\n    fn mark(self) -> Int\n\nimpl Mark for Holder('a):\n    fn mark(self) -> Int:\n        0\n\nfn erase(owner: &'a String, holder: Holder('a)) -> dyn Mark:\n    holder as dyn Mark\n";
    let error = check_str(source).expect_err("an admitted reference carrier cannot erase its loan");
    assert!(error.contains("erasure to `dyn Mark`") && error.contains("owner/lifetime relation"), "{error}");
}

