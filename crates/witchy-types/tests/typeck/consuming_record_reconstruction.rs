//! Reconstructing an affine record transfers its stored obligations once.

use super::source;

const PRELUDE: &str = r#"
must type Ticket:
    Ticket(Int)

type Box(a):
    value: a
    count: Int

fn finish(own box: Box(Ticket)) -> Int:
    match move box:
        Box(ticket, count) -> match move ticket:
            Ticket(value) -> value * 10 + count
"#;

#[test]
fn record_reconstruction_transfers_untouched_generic_resource_fields() {
    for mode in ["", "mode opt\n"] {
        let program = format!("{mode}{PRELUDE}\nfn bump[a](own box: Box(a)) -> Box(a):\n    let count = box.count + 1\n    Box(count: count, ..move box)\n\nfn main() -> Int:\n    finish(bump(Box(Ticket(7), 2)))\n");
        source::checked(&program).expect("unchanged resource moves into the replacement record");
    }
}

#[test]
fn record_reconstruction_cannot_discard_or_duplicate_stored_resources() {
    for mode in ["", "mode opt\n"] {
        for body in [
            "    let box = Box(Ticket(7), 2)\n    let replacement = Box(value: Ticket(9), ..box)\n    finish(replacement)\n",
            "    let box = Box(Ticket(7), 2)\n    let replacement = Box(count: 3, ..box)\n    finish(replacement) + finish(box)\n",
        ] {
            let program = format!("{mode}{PRELUDE}\nfn main() -> Int:\n{body}");
            let error = source::checked(&program).expect_err("spread cannot lose or duplicate Ticket");
            assert!(error.contains("must-consume") || error.contains("consumed") || error.contains("moved"), "{error}");
        }
        let program = format!("{mode}{PRELUDE}\nfn replace(own box: Box(Ticket)) -> Box(Ticket):\n    Box(value: Ticket(9), ..move box)\n\nfn main() -> Int:\n    finish(replace(Box(Ticket(7), 2)))\n");
        let error = source::checked(&program).expect_err("owned entry does not discharge an overwritten child");
        assert!(error.contains("must-consume"), "{error}");
        let program = format!("{mode}{PRELUDE}\nfn duplicate(own box: Box(Ticket)) -> Int:\n    let replacement = Box(count: 3, ..move box)\n    finish(replacement) + finish(box)\n\nfn main() -> Int:\n    duplicate(Box(Ticket(7), 2))\n");
        let error = source::checked(&program).expect_err("reconstruction consumes the entry shell exactly once");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
        let program = format!("{mode}{PRELUDE}\nfn duplicate(own box: Box(Ticket)) -> Box(Ticket):\n    Box(count: finish(box), ..move box)\n\nfn main() -> Int:\n    finish(duplicate(Box(Ticket(7), 2)))\n");
        let error = source::checked(&program).expect_err("an override cannot consume the later spread operand");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
        let program = format!("{mode}{PRELUDE}\nfn duplicate(own box: Box(Ticket)) -> Box(Ticket):\n    Box(count: 3, count: 4, ..move box)\n\nfn main() -> Int:\n    finish(duplicate(Box(Ticket(7), 2)))\n");
        let error = source::checked(&program).expect_err("each replacement slot is selected exactly once");
        assert!(error.contains("field `count` is set twice"), "{error}");
        let program = format!("{mode}{PRELUDE}\nfn replace[a](own box: Box(a), own replacement: a) -> Box(a):\n    Box(value: replacement, ..move box)\n\nfn main() -> Int:\n    finish(replace(Box(Ticket(7), 2), Ticket(9)))\n");
        let error = source::checked(&program).expect_err("instantiation cannot hide an overwritten must child");
        assert!(error.contains("must-consume"), "{error}");
        let program = format!("{mode}{PRELUDE}\nfn replace(own box: Box(Box(Ticket))) -> Box(Box(Ticket)):\n    Box(value: Box(Ticket(9), 4), ..move box)\n\nfn main() -> Int:\n    match replace(Box(Box(Ticket(7), 2), 3)):\n        Box(inner, count) -> finish(inner) + count\n");
        let error = source::checked(&program).expect_err("nested must fields retain their disposition requirement");
        assert!(error.contains("must-consume"), "{error}");
    }
}
