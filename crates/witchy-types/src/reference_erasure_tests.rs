use super::check_str;

#[test]
fn canonical_reference_nominal_cannot_erase_into_owned_existential() {
    let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\ntrait Mark:\n    fn mark(self) -> Int\n\nimpl Mark for Holder('a):\n    fn mark(self) -> Int:\n        0\n\nfn erase(owner: &'a String, holder: Holder('a)) -> dyn Mark:\n    holder as dyn Mark\n";
    let error = check_str(source).expect_err("an admitted reference carrier cannot erase its loan");
    assert!(error.contains("erasure to `dyn Mark`") && error.contains("owner/lifetime relation"), "{error}");
}

#[test]
fn canonical_reference_nominal_requires_an_explicit_lambda_result_relation() {
    for result in ["holder", "(holder, 1)"] {
        let source = format!(
            "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nfn outer():\n    let callback = fn(owner: &'a String, holder: Holder('a)):\n        {result}\n    let _ = callback\n"
        );
        let error = check_str(&source).expect_err("an inferred callable must not hide its borrowed result");
        assert!(error.contains("inferred closure result") && error.contains("owner/lifetime relation"), "{source}\n{error}");
    }
}

#[test]
fn canonical_reference_nominal_transport_preserves_explicit_relations() {
    check_str(
        "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nfn forward(owner: &'a String, holder: Holder('a)) -> Holder('a):\n    let copy = holder\n    copy\n\nfn pair(owner: &'a String, holder: Holder('a)) -> (Holder('a), Int):\n    (holder, 1)\n"
    ).expect("the erasure guard does not re-ban nominal, tuple, or variable transport");
}
