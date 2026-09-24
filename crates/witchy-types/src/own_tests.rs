// These tests preserve historical ownership behavior for the migration-only
// reference grammar; ordinary source continues to use `check_str`.
use super::check_reference_migration_str as check_str;

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

