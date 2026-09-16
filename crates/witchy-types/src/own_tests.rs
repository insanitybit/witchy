use super::check_str;

#[test]
fn own_consumes_ordinary_bindings_in_both_modes_and_call_shapes() {
    for mode in ["", "mode opt\n\n"] {
        for setup in ["", "    let invoke = take\n"] {
            let call = if setup.is_empty() { "take" } else { "invoke" };
            let source = format!(
                "{mode}fn take(own text: String):\n    let _ = text\n\nfn main():\n{setup}    let text = \"hello\"\n    {call}(text)\n    let _ = text\n"
            );
            let error = check_str(&source).expect_err("own consumes in every mode and call shape");
            assert!(error.contains("use of `text` after it was moved"), "{source}\n{error}");
        }
    }
}

#[test]
fn own_transfer_preserves_snapshots_and_permits_reinitialization() {
    for mode in ["", "mode opt\n\n"] {
        check_str(&format!(
            "{mode}fn take(own text: String) -> String:\n    text\n\nfn exercise() -> String:\n    var text = \"hello\"\n    let snapshot = text\n    let result = take(text)\n    text = \"again\"\n    let _ = snapshot\n    let _ = result\n    text\n"
        )).expect("transferring one binding does not consume its prior snapshots or prevent reassignment");
    }
}

#[test]
fn default_owned_parameters_still_allow_copyable_caller_reuse() {
    for mode in ["", "mode opt\n\n"] {
        check_str(&format!(
            "{mode}fn read(text: String):\n    let _ = text\n\nfn exercise() -> String:\n    let text = \"hello\"\n    read(text)\n    text\n"
        )).expect("the consuming own convention does not redefine default snapshot arguments");
    }
}

#[test]
fn own_reference_handle_consumption_does_not_depend_on_result_annotation() {
    for mode in ["", "mode opt\n\n"] {
        for kind in ["", "mut "] {
            for result in ["", " -> ()"] {
                for setup in ["", "    let invoke = take\n"] {
                    let call = if setup.is_empty() { "take" } else { "invoke" };
                    let source = format!(
                        "{mode}fn take(own value: &'a {kind}String){result}:\n    return\n\nfn caller(value: &'a {kind}String):\n{setup}    {call}(value)\n    {call}(value)\n"
                    );
                    let error = check_str(&source).expect_err("a transferred reference handle is unavailable");
                    assert!(error.contains("use of `value` after it was moved"), "{source}\n{error}");
                    let single = source.replacen(&format!("    {call}(value)\n    {call}(value)"), &format!("    {call}(value)"), 1);
                    check_str(&single).unwrap_or_else(|error| panic!("one handle transfer must succeed\n{single}\n{error}"));
                }
            }
        }
    }
}

#[test]
fn own_generic_reference_without_result_annotation_preserves_its_input_relation() {
    for kind in ["", "mut "] {
        check_str(&format!(
            "mode opt\n\nfn take(own value: &'a {kind}a):\n    return\n\nfn main():\n    var text = \"hello\"\n    let handle = &{kind}text\n    take(handle)\n    text = \"again\"\n"
        )).expect("result inference cannot erase a generic reference parameter");
    }
}

#[test]
fn moving_shared_handle_preserves_other_shared_handles_and_owner() {
    check_str(
        "mode opt\n\nfn take(own value: &'a String) -> &'a String:\n    value\n\nfn main():\n    var text = \"hello\"\n    let handle = &text\n    let other = handle\n    let transferred = take(move handle)\n    let _ = *other\n    let _ = *transferred\n    text = \"again\"\n"
    ).expect("moving a shared handle neither consumes its referent nor independent shared handles");

    check_str(
        "mode opt\n\nfn take(own value: &'a String):\n    return\n\nfn caller(value: &'a String) -> &'a String:\n    let saved = value\n    take(value)\n    saved\n"
    ).expect("a reference parameter's referent is not owned by the consumed handle");
}

#[test]
fn moving_a_reference_handle_cannot_erase_its_live_owner_relation() {
    for kind in ["", "mut "] {
        let source = format!(
            "mode opt\n\nfn main():\n    var text = \"hello\"\n    let handle = &{kind}text\n    let transferred = move handle\n    text = \"invalid\"\n    let _ = *transferred\n"
        );
        let error = check_str(&source).expect_err("moving a handle retains its live loan");
        assert!(error.contains("reassigned") || error.contains("exclusive"), "{source}\n{error}");
    }
}

#[test]
fn own_cannot_move_a_shared_handle_while_its_slot_is_borrowed() {
    let error = check_str(
        "mode opt\n\nfn take(own value: &'a String):\n    return\n\nfn caller(value: &'a String):\n    let saved = &value\n    take(value)\n    let _ = *saved\n"
    ).expect_err("copying a referent handle differs from borrowing the handle slot");
    assert!(error.contains("still live"), "{error}");
}

#[test]
fn own_shared_handle_replacement_requires_a_reference_relation() {
    let error = check_str(
        "mode opt\n\nfn wrong(own value: &'a String) -> &'a String:\n    value = \"owned\"\n    value\n"
    ).expect_err("own grants local handle ownership, not a conversion from owned storage to a reference");
    assert!(error.contains("assignment to shared reference handle `value`"), "{error}");

    check_str(
        "mode opt\n\nfn replace(own value: &'a String, other: &'a String) -> &'a String:\n    value = other\n    value\n"
    ).expect("an owned shared handle may be replaced by a relation-preserving handle");
}

#[test]
fn own_cannot_transfer_the_same_binding_twice_in_one_call() {
    for mode in ["", "mode opt\n\n"] {
        for setup in ["", "    let invoke = take\n"] {
            let call = if setup.is_empty() { "take" } else { "invoke" };
            let source = format!(
                "{mode}fn take(own left: String, own right: String):\n    return\n\nfn main():\n{setup}    let text = \"hello\"\n    {call}(text, text)\n"
            );
            let error = check_str(&source).expect_err("one binding cannot supply two consuming arguments");
            assert!(error.contains("moved") || error.contains("transferred"), "{source}\n{error}");
        }
    }
}

#[test]
fn own_method_and_existential_arguments_consume_normal_bindings() {
    for receiver in ["Runner", "Runner as dyn Consume"] {
        let source = format!(
            "trait Consume:\n    fn take(self, own text: String)\n\ntype Runner:\n    Runner\n\nimpl Consume for Runner:\n    fn take(self, own text: String):\n        return\n\nfn main():\n    let runner = {receiver}\n    let text = \"hello\"\n    runner.take(text)\n    let _ = text\n"
        );
        let error = check_str(&source).expect_err("method dispatch cannot erase own consumption");
        assert!(error.contains("use of `text` after it was moved"), "{source}\n{error}");
    }
}
