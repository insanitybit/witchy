use super::source;

const WORKER: &str = r#"must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
pub fn consume(own token: Box(Ticket), value: Int) -> Result(Int, String):
    match move token:
        Box(ticket) -> match move ticket:
            Ticket(_) -> Ok(value)
"#;

#[test]
fn later_operand_cannot_abandon_an_owned_resource() {
    for (root_mode, worker_mode, local) in [
        ("", "", false), ("mode opt\n", "mode opt\n", false),
        ("", "mode opt\n", false), ("", "mode opt\n", true),
    ] {
        let worker = format!("{worker_mode}{WORKER}");
        let mut main = format!(r#"{root_mode}import worker
fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn choose(bad: Bool) -> Result(Int, String):
    worker.consume(worker.Box(worker.Ticket(7)), maybe(bad)?)
fn main() -> Int:
    choose(true) ?? 0
"#);
        if local {
            main = main.replace("worker.consume(", "consume(");
            main.push_str("\nfn consume(own token: worker.Box(worker.Ticket), value: Int) -> Result(Int, String):\n    match move token:\n        worker.Box(ticket) -> match move ticket:\n            worker.Ticket(_) -> Ok(value)\n");
        }
        let error = source::checked_bundle(vec![("main", &main), ("worker", &worker)], "main")
            .err().expect("a later error edge abandons the earlier owned operand");
        assert!(format!("{error:?}").contains("already evaluated must-consume operand"), "{error:?}");
    }
}

#[test]
fn pending_operands_cover_indirect_calls_and_recursive_aggregates() {
    for mode in ["", "mode opt\n"] {
        for expression in [
            "consume(Ticket(7), maybe()?)",
            "callback(Ticket(7), maybe()?)",
            "(if true: consume else: consume)(Ticket(7), maybe()?)",
            "finish_pair(Pair(Ticket(7), maybe()?))",
            "finish_tuple((Ticket(7), maybe()?))",
            "finish_list([Ticket(7), make(maybe()?)])",
        ] {
            let input = format!(r#"{mode}must type Ticket:
    Ticket(Int)
type Pair(a, b):
    Pair(a, b)
fn maybe() -> Result(Int, String):
    Err("stop")
fn make(x: Int) -> Ticket:
    Ticket(x)
fn consume(own t: Ticket, x: Int) -> Result(Int, String):
    match move t:
        Ticket(_) -> Ok(x)
fn finish_pair(own t: Pair(Ticket, Int)) -> Result(Int, String):
    match move t:
        Pair(ticket, _) -> match move ticket:
            Ticket(_) -> Ok(1)
fn finish_tuple(own t: (Ticket, Int)) -> Result(Int, String):
    match move t:
        (ticket, _) -> match move ticket:
            Ticket(_) -> Ok(1)
fn finish_list(own t: List(Ticket)) -> Result(Int, String):
    for ticket in move t:
        match move ticket:
            Ticket(_) -> 0
    Ok(1)
fn choose() -> Result(Int, String):
    let callback = consume
    {expression}
fn main() -> Int:
    choose() ?? 0
"#);
            let error = source::checked_bundle(vec![("main", &input)], "main")
                .err().expect("pending resource must not disappear");
            assert!(format!("{error:?}").contains("already evaluated must-consume operand"), "{expression}: {error:?}");
        }
    }
}

#[test]
fn structured_exit_checks_only_operands_in_the_exited_context() {
    for mode in ["", "mode opt\n"] {
        for (body, rejected) in [
            ("    consume(Ticket(7), if true:\n        return Ok(1)\n    else:\n        9\n    )\n", true),
            ("    while true:\n        let _ = consume(Ticket(7), if true:\n            break\n        else:\n            9\n        )\n    Ok(1)\n", true),
            ("    consume(Ticket(7), if true:\n        while true:\n            break\n        9\n    else:\n        9\n    )\n", false),
            ("    let token = Ticket(7)\n    consume(move token, maybe()?)\n", true),
        ] {
            let input = format!(r#"{mode}must type Ticket:
    Ticket(Int)
fn consume(own token: Ticket, x: Int) -> Result(Int, String):
    match move token:
        Ticket(_) -> Ok(x)
fn maybe() -> Result(Int, String):
    Err("stop")
fn choose() -> Result(Int, String):
{body}fn main() -> Int:
    choose() ?? 0
"#);
            let result = source::checked_bundle(vec![("main", &input)], "main");
            if rejected {
                let error = result.err().expect("exit abandons an evaluated resource operand");
                assert!(format!("{error:?}").contains("already evaluated must-consume operand"), "{body}: {error:?}");
            } else {
                result.expect("inner-loop exit does not abandon the enclosing call operand");
            }
        }
    }
}

#[test]
fn borrowed_operands_and_the_try_payload_are_not_pending_owners() {
    for mode in ["", "mode opt\n"] {
        let input = format!(r#"{mode}must type Ticket:
    Ticket(Int)
fn maybe() -> Result(Int, String):
    Ok(9)
fn read(let token: Ticket, x: Int) -> Result(Int, String):
    Ok(x)
fn borrowed(let token: Ticket) -> Result(Int, String):
    read(token, maybe()?)
fn error_payload() -> Result(Int, Ticket):
    Err(Ticket(7))
fn relay() -> Result(Int, Ticket):
    Ok(error_payload()?)
fn consume(own token: Ticket, callback: fn() -> Result(Int, String)) -> Result(Int, String):
    match move token:
        Ticket(_) -> callback()
fn deferred() -> Result(Int, String):
    consume(Ticket(7), fn() -> Result(Int, String):
        Ok(maybe()?)
    )
fn main() -> Int:
    deferred() ?? 0
"#);
        source::checked_bundle(vec![("main", &input)], "main").expect("borrow, transferred error payload and deferred body are distinct from pending ownership");
    }
}

#[test]
fn suspension_cannot_abandon_a_completed_resource_argument() {
    for mode in ["", "mode opt\n"] {
        let input = format!(r#"{mode}must type Ticket:
    Ticket(Int)
fn consume(own token: Ticket, x: Int) -> Int:
    match move token:
        Ticket(_) -> x
async fn value() -> Int:
    9
async fn main():
    let _ = consume(Ticket(7), value().await)
"#);
        let error = source::checked_bundle(vec![("main", &input)], "main")
            .err().expect("suspension precedes the resource's consuming call");
        assert!(format!("{error:?}").contains("already evaluated must-consume operand"), "{error:?}");
    }
}

#[test]
fn trailing_base_cleanup_cannot_discharge_an_earlier_override_exit() {
    for (root_mode, worker_mode, local) in [
        ("", "", false), ("mode opt\n", "mode opt\n", false),
        ("", "mode opt\n", false), ("", "mode opt\n", true),
    ] {
        let worker = format!(r#"{worker_mode}must type Ticket:
    Ticket(Int)
type Box:
    count: Int
pub fn cleanup(own token: Ticket) -> Box:
    match move token:
        Ticket(_) -> Box(7)
"#);
        let mut input = format!(r#"{root_mode}import worker
fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn choose(bad: Bool) -> Result(worker.Box, String):
    let token = worker.Ticket(7)
    Ok(worker.Box(count: maybe(bad)?, ..worker.cleanup(token)))
fn main() -> Int:
    0
"#);
        if local {
            input = input.replace("worker.cleanup(", "cleanup(");
            input.push_str("\nfn cleanup(own token: worker.Ticket) -> worker.Box:\n    match move token:\n        worker.Ticket(_) -> worker.Box(7)\n");
        }
        let error = source::checked_bundle(vec![("main", &input), ("worker", &worker)], "main")
            .err().expect("the override returns before the trailing cleanup executes");
        assert!(format!("{error:?}").contains("must-consume value `token` undisposed"), "{error:?}");
    }
}
