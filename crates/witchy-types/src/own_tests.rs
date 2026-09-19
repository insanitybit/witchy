// These tests preserve historical ownership behavior for the migration-only
// reference grammar; ordinary source continues to use `check_str`.
use super::check_reference_migration_str as check_str;

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
    for kind in ["", "mut "] {
        for result in ["", " -> ()"] {
            for setup in ["", "    let invoke = take\n"] {
                let call = if setup.is_empty() { "take" } else { "invoke" };
                let source = format!(
                    "mode opt\n\nfn take(own value: &'a {kind}String){result}:\n    return\n\nfn caller(value: &'a {kind}String):\n{setup}    {call}(value)\n    {call}(value)\n"
                );
                let error = check_str(&source).expect_err("a transferred reference handle is unavailable");
                assert!(error.contains("use of `value` after it was moved"), "{source}\n{error}");
                let single = source.replacen(&format!("    {call}(value)\n    {call}(value)"), &format!("    {call}(value)"), 1);
                check_str(&single).unwrap_or_else(|error| panic!("one handle transfer must succeed\n{single}\n{error}"));
            }
        }
    }
}

#[test]
fn own_reference_handles_do_not_bypass_opt_only_reference_syntax() {
    for kind in ["", "mut "] {
        let error = check_str(&format!(
            "fn take(own value: &'a {kind}String):\n    return\n"
        )).expect_err("own does not admit explicit-reference syntax in normal mode");
        assert!(error.contains("available only in `mode opt` files"), "{error}");
    }
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

#[test]
fn closure_value_captures_have_private_move_state_in_both_modes() {
    for mode in ["", "mode opt\n\n"] {
        for qualifier in ["fn", "once fn"] {
            for argument in ["text", "move text"] {
                let again = if qualifier == "fn" { "    let second = callback()\n" } else { "" };
                let source = format!(
                    "{mode}fn take(own text: String) -> String:\n    text\n\nfn exercise() -> String:\n    let text = \"hello\"\n    let callback = {qualifier}() -> String:\n        take({argument})\n    let before = text\n    let first = callback()\n{again}    text\n"
                );
                check_str(&source).unwrap_or_else(|error| panic!("a closure owns its snapshot, not the outer binding\n{source}\n{error}"));
            }
        }
    }
}

#[test]
fn closure_snapshot_scope_does_not_hide_real_moves() {
    for mode in ["", "mode opt\n\n"] {
        for body in [
            "    let callback = fn() -> String:\n        let first = take(text)\n        text\n    let _ = callback\n",
            "    let first = take(text)\n    let callback = fn() -> String: text\n    let _ = callback\n",
        ] {
            let source = format!(
                "{mode}fn take(own text: String) -> String:\n    text\n\nfn exercise():\n    let text = \"hello\"\n{body}"
            );
            let error = check_str(&source).expect_err("a snapshot cannot revive a moved source or ignore its own body move");
            assert!(error.contains("moved"), "{source}\n{error}");
        }

        let transferred = format!(
            "{mode}fn take(own text: String) -> String:\n    text\n\nfn run(own callback: fn() -> String) -> String:\n    callback()\n\nfn exercise():\n    let text = \"hello\"\n    let callback = fn() -> String: take(text)\n    let first = run(callback)\n    let second = callback()\n"
        );
        let error = check_str(&transferred).expect_err("own still transfers the closure binding itself");
        assert!(error.contains("moved"), "{transferred}\n{error}");

        let invoked = format!(
            "{mode}fn take(own text: String) -> String:\n    text\n\nfn exercise():\n    let text = \"hello\"\n    let callback = once fn() -> String: take(text)\n    let first = callback()\n    let second = callback()\n"
        );
        let error = check_str(&invoked).expect_err("once invocation still consumes the closure binding");
        assert!(error.contains("once") || error.contains("moved"), "{invoked}\n{error}");
    }
}

#[test]
fn closure_snapshot_scopes_preserve_must_and_once_capture_rejections() {
    for mode in ["", "mode opt\n\n"] {
        for qualifier in ["fn", "once fn"] {
            let must = format!(
                "{mode}must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    1\n\nfn exercise():\n    let ticket = Ticket(1)\n    let callback = {qualifier}() -> Int: finish(ticket)\n    let _ = callback\n"
            );
            let error = check_str(&must).expect_err("an opaque closure cannot hide a must-consume capture");
            assert!(error.contains("closure environment carries must-consume `ticket`"), "{must}\n{error}");

            let once = format!(
                "{mode}fn exercise():\n    let action = once fn() -> Int: 1\n    let callback = {qualifier}() -> Int: action()\n    let _ = callback\n"
            );
            let error = check_str(&once).expect_err("an affine callable cannot become a copied capture");
            assert!(error.contains("captures affine once-callable `action`"), "{once}\n{error}");
        }
    }
}

#[test]
fn closure_scope_isolation_preserves_explicit_own_but_rejects_late_inferred_must() {
    // An explicitly must-typed own parameter retains its transfer identity and
    // can be discharged by the continuation. A generic snapshot that only
    // becomes must-consume during body inference cannot acquire that authority
    // retroactively.
    for mode in ["", "mode opt\n\n"] {
        let source = format!(
            "{mode}must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    1\n\nfn defer(own ticket: Ticket) -> fn() -> Int:\n    fn() -> Int: finish(ticket)\n"
        );
        check_str(&source).unwrap_or_else(|error| panic!("existing discharged-own continuation contract must not change here\n{source}\n{error}"));

        let inferred = format!(
            "{mode}must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    1\n\nfn defer() -> fn(own Ticket) -> fn() -> Int:\n    fn(own ticket):\n        fn() -> Int: finish(ticket)\n"
        );
        let error = check_str(&inferred)
            .expect_err("late inference cannot hide a must-consume value in an ordinary snapshot");
        assert!(
            error.contains("closure environment carries must-consume `ticket`"),
            "{inferred}\n{error}"
        );
    }
}

#[test]
fn inferred_capture_types_cannot_reveal_hidden_once_obligations() {
    for (mode, expected) in [
        ("", "captures affine once-callable `value`"),
        ("mode opt\n\n", "requires an opt-compatible callable"),
    ] {
        let once = format!(
            "{mode}fn invoke(own action: once fn() -> Int) -> Int:\n    action()\n\nfn defer(own value: a) -> fn() -> Int:\n    fn() -> Int: invoke(value)\n"
        );
        let error = check_str(&once).expect_err("body inference cannot hide an affine callable in a copied capture");
        assert!(error.contains(expected), "{once}\n{error}");
    }
}

#[test]
fn closure_snapshot_scopes_retain_frozen_storage_restrictions() {
    let source = "mode opt\n\nfn make(let values: frozen List(Int)):\n    let callback = fn():\n        let reference = &mut values\n        let _ = reference\n    let _ = callback\n";
    let error = check_str(source).expect_err("a private capture does not remove its frozen storage contract");
    assert!(error.contains("frozen"), "{error}");
}
