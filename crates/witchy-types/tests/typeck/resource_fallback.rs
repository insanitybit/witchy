//! Resource disposition at the checked source fallback boundary.

use super::source;

#[test]
fn fallback_cannot_discard_a_resource_error() {
    for mode in ["", "mode opt\n"] {
        for (declarations, error_type, error_value) in [
            ("must type Ticket:\n    Ticket(Int)", "Ticket", "Ticket(7)"),
            ("must type Ticket:\n    Ticket(Int)\n\ntype Box(a):\n    Box(a)", "Box(List(Ticket))", "Box([Ticket(7)])"),
            ("must type Ticket:\n    Ticket", "Ticket", "Ticket"),
        ] {
            let program = format!("{mode}\n{declarations}\n\nfn failed() -> Result(Int, {error_type}):\n    Err({error_value})\n\nfn main() -> Int:\n    failed() ?? 0\n");
            assert_resource_error(source::checked(&program));
        }
    }
}

fn assert_resource_error(result: Result<(), String>) {
    let error = result.expect_err("the reachable resource error must not disappear into fallback");
    assert!(error.contains("`??` cannot discard must-consume error"), "{error}");
}

#[test]
fn fallback_preserves_resource_errors_across_opt_calls_and_imported_types() {
    let fast = "mode opt\n\nmust type Ticket:\n    Ticket(Int)\n\npub fn failed() -> Result(Int, Ticket):\n    Err(Ticket(7))\n";
    for main in [
        "import fast\n\nfn main() -> Int:\n    fast.failed() ?? 0\n",
        "import fast\n\nfn failed() -> Result(Int, fast.Ticket):\n    Err(fast.Ticket(7))\n\nfn main() -> Int:\n    failed() ?? 0\n",
    ] {
        assert_resource_error(source::checked_bundle(vec![("fast", fast), ("main", main)], "main").map(drop));
    }
}

#[test]
fn generic_fallback_rechecks_the_instantiated_error_obligation() {
    for mode in ["", "mode opt\n"] {
        let program = format!("{mode}\nmust type Ticket:\n    Ticket(Int)\n\nfn recover[a](own input: Result(Int, a)) -> Int:\n    input ?? 0\n\nfn failed() -> Result(Int, Ticket):\n    Err(Ticket(7))\n\nfn main() -> Int:\n    recover(failed())\n");
        assert_resource_error(source::checked(&program));
    }
}

#[test]
fn fallback_keeps_success_obligations_and_ignores_nonstored_mentions() {
    for mode in ["", "mode opt\n"] {
        let prelude = format!("{mode}\nmust type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    match ticket:\n        Ticket(value) -> value\n\nfn success(flag: Bool) -> Result(Ticket, String):\n    if flag:\n        Ok(Ticket(7))\n    else:\n        Err(\"retry\")\n");
        source::checked(&format!("{prelude}\nfn main() -> Int:\n    let ticket = success(true) ?? Ticket(9)\n    finish(ticket)\n"))
            .expect("fallback transfers or constructs the success resource");
        let error = source::checked(&format!("{prelude}\nfn main():\n    let ticket = success(true) ?? Ticket(9)\n"))
            .expect_err("transferred success resource still needs disposition");
        assert!(error.contains("must-consume value `ticket`"), "{error}");
        for (declarations, error_type, error_value) in [
            ("type Phantom(a):\n    Phantom(Int)", "Phantom(Ticket)", "Phantom(7)"),
            ("fn make() -> Ticket:\n    Ticket(7)", "fn() -> Ticket", "make"),
        ] {
            source::checked(&format!("{prelude}\n{declarations}\n\nfn failed() -> Result(Int, {error_type}):\n    Err({error_value})\n\nfn main() -> Int:\n    failed() ?? 0\n"))
                .expect("a phantom argument or callable signature stores no Ticket");
        }
    }
}
