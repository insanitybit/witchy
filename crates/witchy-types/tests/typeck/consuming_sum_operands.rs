//! Consuming sum extraction transfers the owner, not just the returned payload.

const RESOURCE: &str = r#"
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n
"#;

#[test]
fn resource_sum_extraction_cannot_reuse_its_input() {
    for mode in ["", "mode opt\n"] {
        for expression in ["value ?? Ticket(9)", "value?"] {
            let source = format!("{mode}{RESOURCE}\nfn duplicate(own value: Result(Ticket, String)) -> Result(Int, String):\n    let first = {expression}\n    let second = {expression}\n    Ok(finish(first) + finish(second))\n");
            let error = super::source::checked(&source).expect_err("extracting the first payload transfers the input owner");
            assert!(error.contains("value") && error.contains("after")
                && (error.contains("moved") || error.contains("transferred")), "{error}");
        }
    }
}

#[test]
fn fallback_cannot_reuse_its_consumed_left_input() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{RESOURCE}\nfn nested(own value: Option(Ticket)) -> Ticket:\n    value ?? (value ?? Ticket(9))\n");
        let error = super::source::checked(&source).expect_err("the left wrapper is consumed before its fallback executes");
        assert!(error.contains("value") && error.contains("after")
            && (error.contains("moved") || error.contains("transferred")), "{error}");
    }
}

#[test]
fn generic_extraction_rechecks_instantiated_storage() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}{RESOURCE}
fn duplicate[a](own value: Result(a, String)) -> Result((a, a), String):
    let first = value?
    let second = value?
    Ok((first, second))

fn finish_pair(own value: Result((Ticket, Ticket), String)) -> Int:
    match value:
        Ok((first, second)) -> finish(first) + finish(second)
        Err(_) -> 0

fn main() -> Int:
    finish_pair(duplicate(Ok(Ticket(7))))
"#);
        let error = super::source::checked(&source).expect_err("generic extraction cannot duplicate its instantiated resource payload");
        assert!(error.contains("value") && error.contains("after")
            && (error.contains("moved") || error.contains("transferred")), "{error}");
    }
}

#[test]
fn extraction_requires_the_whole_owned_resource() {
    for mode in ["", "mode opt\n"] {
        for (declaration, expected) in [
            ("fn steal(let value: Result(Ticket, String)) -> Ticket:\n    value ?? Ticket(9)\n", "cannot consume borrowed must-consume value `value`"),
            ("type Holder:\n    value: Option(Ticket)\n\nfn steal(own holder: Holder) -> Ticket:\n    holder.value ?? Ticket(9)\n", "cannot partially consume `holder`"),
        ] {
            let error = super::source::checked(&format!("{mode}{RESOURCE}\n{declaration}"))
                .expect_err("borrowed or partial resource extraction needs an ownership transfer");
            assert!(error.contains(expected), "{error}");
        }
    }
}

#[test]
fn imported_resource_identity_does_not_enable_repeated_extraction() {
    let fast = "mode opt\nmust type Ticket:\n    Ticket(Int)\n\npub fn finish(own ticket: Ticket) -> Int:\n    match ticket:\n        Ticket(n) -> n\n";
    let main = r#"import fast
fn duplicate(own value: Result(fast.Ticket, String)) -> Int:
    let first = value ?? fast.Ticket(9)
    let second = value ?? fast.Ticket(9)
    fast.finish(first) + fast.finish(second)

fn main() -> Int:
    duplicate(Ok(fast.Ticket(7)))
"#;
    let error = super::source::checked_bundle(vec![("fast", fast), ("main", main)], "main")
        .expect_err("an opt-defined payload retains the same owner-transfer rules");
    assert!(error.contains("value") && error.contains("after")
        && (error.contains("moved") || error.contains("transferred")), "{error}");
}

#[test]
fn once_error_storage_requires_explicit_whole_wrapper_transfer() {
    for mode in ["", "mode opt\n"] {
        let declaration = "fn propagate(own value: Result(Int, once fn() -> Int)) -> Result(Int, once fn() -> Int):\n";
        let source = format!("{mode}{declaration}    let n = value?\n    Ok(n)\n");
        let error = super::source::checked(&source).expect_err("the error arm also stores affine callable state");
        assert!(error.contains("once") && error.contains("move"), "{error}");
        super::source::checked(&format!("{mode}{declaration}    let n = (move value)?\n    Ok(n)\n"))
            .expect("an explicit transfer retires the complete once-bearing wrapper");
    }
}

#[test]
fn extraction_consumes_only_affine_storage_and_keeps_independent_fallbacks() {
    for mode in ["", "mode opt\n"] {
        let source = format!(r#"{mode}{RESOURCE}
fn ordinary(value: Option(Int)) -> Int:
    (value ?? 2) + (value ?? 3)

fn fallback(own value: Option(Ticket)) -> Ticket:
    value ?? Ticket(9)

fn propagate(own value: Result(Ticket, String)) -> Result(Int, String):
    let ticket = value?
    Ok(finish(ticket))

fn explicit() -> Int:
    let value = Some(Ticket(7))
    let ticket = (move value) ?? Ticket(9)
    finish(ticket)
"#);
        super::source::checked(&source).expect("ordinary snapshots and single ownership transfers remain valid");
    }
}
