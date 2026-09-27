//! A conditional transfer cannot discharge the owner on the skipped edge.

const RESOURCE: &str = r#"
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(n) -> n

fn finish_bool(own ticket: Ticket) -> Bool:
    finish(ticket) > 0
"#;

#[test]
fn skipped_lazy_operands_preserve_live_resource_obligations() {
    for mode in ["", "mode opt\n"] {
        for expression in [
            "Some(1) ?? finish(ticket)",
            "false && finish_bool(ticket)",
            "true || finish_bool(ticket)",
        ] {
            let source = format!("{mode}{RESOURCE}\nfn main():\n    let ticket = Ticket(7)\n    let _ = {expression}\n");
            let error = super::source::checked(&source)
                .expect_err("a skipped RHS leaves the Ticket obligation live");
            assert!(error.contains("must-consume value `ticket`"), "{error}");
        }
    }
}

#[test]
fn lazy_local_creation_and_unconditional_disposition_remain_valid() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{RESOURCE}\nfn main() -> Int:\n    let ticket = Ticket(7)\n    let first = Some(finish(ticket)) ?? 9\n    let second = Some(1) ?? finish(Ticket(9))\n    let third = false && finish_bool(Ticket(9))\n    let fourth = true || finish_bool(Ticket(9))\n    first + second\n");
        super::source::checked(&source)
            .expect("left-side disposition always runs; RHS-only resources exist only if it runs");
    }
}

#[test]
fn a_possible_rhs_transfer_cannot_be_used_again_after_the_join() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{RESOURCE}\nfn choose(flag: Bool) -> Int:\n    let ticket = Ticket(7)\n    let _ = flag && finish_bool(ticket)\n    finish(ticket)\n");
        let error = super::source::checked(&source)
            .expect_err("the RHS may already have consumed this owner");
        assert!(error.contains("consumed") || error.contains("transferred"), "{error}");
    }
}

#[test]
fn skipped_transfers_remain_live_across_opt_module_boundaries() {
    let fast = format!("mode opt\n{RESOURCE}").replace("fn finish", "pub fn finish");
    for expression in [
        "Some(1) ?? fast.finish(ticket)",
        "false && fast.finish_bool(ticket)",
        "true || fast.finish_bool(ticket)",
    ] {
        let main = format!("import fast\n\nfn main():\n    let ticket = fast.Ticket(7)\n    let _ = {expression}\n");
        let error = super::source::checked_bundle(vec![("fast", &fast), ("main", &main)], "main")
            .expect_err("opt-defined type and callee do not discharge a skipped transfer");
        assert!(error.contains("must-consume value `ticket`"), "{error}");
    }
}

#[test]
fn possible_once_invocation_cannot_be_repeated_after_lazy_join() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}fn choose(flag: Bool) -> Bool:\n    let action: once fn() -> Bool = once fn(): true\n    let _ = flag && action()\n    action()\n");
        let error = super::source::checked(&source)
            .expect_err("the lazy operand may already have invoked the once callable");
        assert!(error.contains("once") && (error.contains("already") || error.contains("consumed")), "{error}");
    }
}

#[test]
fn terminating_rhs_consumption_does_not_consume_the_skipped_continuation() {
    for mode in ["", "mode opt\n"] {
        for (left, result, finish, leak_tail) in [
            ("flag &&", "Bool", "finish_bool(ticket)", "false"),
            ("flag ||", "Bool", "finish_bool(ticket)", "false"),
            ("Some(1) ??", "Int", "finish(ticket)", "0"),
        ] {
            // Both RHS branches return from choose. Only the skipped edge
            // reaches its tail, where the original owner is still available.
            let prefix = format!("{mode}{RESOURCE}\nfn choose(flag: Bool) -> {result}:\n    let ticket = Ticket(7)\n    let _ = {left} if flag:\n        return {finish}\n    else:\n        return {finish}\n");
            super::source::checked(&format!("{prefix}    {finish}\n"))
                .expect("the skipped continuation may consume its still-live owner exactly once");
            let error = super::source::checked(&format!("{prefix}    {leak_tail}\n"))
                .expect_err("returning on the RHS does not discharge the skipped edge's owner");
            assert!(error.contains("must-consume value `ticket`"), "{error}");
        }
    }
}
