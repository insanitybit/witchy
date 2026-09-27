//! A guard's false edge retains its effects before the next arm is considered.
use super::source;

const WORKER: &str = r#"must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
pub fn finish(own value: Box(Ticket)) -> Bool:
    match move value:
        Box(token) -> match move token:
            Ticket(_) -> false
pub fn inspect(let value: Box(Ticket)) -> Bool:
    false
"#;

const DUPLICATE: &str = r#"import worker
fn main() -> Int:
    let token = worker.Box(worker.Ticket(7))
    match true:
        _ if worker.finish(token) -> 1
        _ -> if worker.finish(token): 2 else: 3
"#;

fn checked(main_mode: &str, worker_mode: &str, main: &str) -> Result<(), String> {
    let main = format!("{main_mode}{main}");
    let worker = format!("{worker_mode}{WORKER}");
    source::checked_bundle(vec![("worker", &worker), ("main", &main)], "main").map(drop)
}

const CONTEXTS: [(&str, &str, bool); 4] = [
    ("", "", false), ("mode opt\n", "mode opt\n", false),
    ("", "mode opt\n", false), ("", "mode opt\n", true),
];

fn local_consumer(main: &str, local: bool) -> String {
    if !local { return main.into(); }
    main.replace("worker.finish", "finish").replace("import worker\n", "import worker\nfn finish(own value: worker.Box(worker.Ticket)) -> Bool:\n    match move value:\n        worker.Box(token) -> match move token:\n            worker.Ticket(_) -> false\n")
}

#[test]
fn false_match_guard_cannot_resurrect_nested_resource() {
    let results = CONTEXTS.map(|(main_mode, worker_mode, local)| {
        let result = checked(main_mode, worker_mode, &local_consumer(DUPLICATE, local));
        result
    });
    for result in results {
        let error = result
            .expect_err("a false guard has already consumed the nested resource before the fallback arm");
        assert!(error.contains("consumed") || error.contains("transferred") || error.contains("match guard"), "{error}");
    }
}

#[test]
fn borrowing_guard_preserves_one_disposition_per_arm() {
    let main = DUPLICATE.replace("_ if worker.finish(token)", "_ if worker.inspect(token)")
        .replace("-> 1", "-> if worker.finish(token): 4 else: 5");
    for (main_mode, worker_mode, local) in CONTEXTS {
        checked(main_mode, worker_mode, &local_consumer(&main, local)).expect("borrow does not consume the caller's nested resource");
    }
}

#[test]
fn unconditional_guard_disposition_survives_fallthrough() {
    let main = DUPLICATE.replace("_ -> if worker.finish(token): 2 else: 3", "_ -> 3");
    for (main_mode, worker_mode, local) in CONTEXTS {
        checked(main_mode, worker_mode, &local_consumer(&main, local))
            .expect("wildcard guard always executes, so neither outcome owes a second disposition");
    }
}

#[test]
fn skipped_pattern_or_lazy_guard_cannot_claim_definite_disposition() {
    let refutable = DUPLICATE.replace("_ if worker.finish(token)", "true if worker.finish(token)");
    let lazy = DUPLICATE.replace("_ if worker.finish(token)", "_ if false && worker.finish(token)")
        .replace("_ -> if worker.finish(token): 2 else: 3", "_ -> 3");
    for mode in ["", "mode opt\n"] {
        for main in [&refutable, &lazy] {
            let error = checked(mode, mode, main).expect_err("an alternative path cannot erase or resurrect an obligation");
            assert!(error.contains("consumed") || error.contains("transferred") || error.contains("must-consume"), "{error}");
        }
    }
}

#[test]
fn guarded_consuming_match_refuses_pattern_payload_fallthrough() {
    for mode in ["", "mode opt\n"] {
        let program = format!("{mode}\n{WORKER}\nfn finish_ticket(own value: Ticket) -> Bool:\n    match move value:\n        Ticket(_) -> false\nfn run(own box: Box(Ticket)) -> Int:\n    match move box:\n        Box(token) if finish_ticket(token) -> 1\n        Box(token) -> if finish_ticket(token): 2 else: 3\nfn main() -> Int:\n    run(Box(Ticket(7)))\n");
        let error = source::checked(&program).expect_err("guarded extraction cannot prove disposition across fallthrough");
        assert!(error.contains("matching would erase a must-consume obligation"), "{error}");
    }
}

#[test]
fn borrowed_pattern_guard_is_fail_closed_but_unguarded_extraction_is_valid() {
    for mode in ["", "mode opt\n"] {
        let program = format!("{mode}\n{WORKER}\nfn finish_ticket(own value: Ticket) -> Int:\n    match move value:\n        Ticket(n) -> n\nfn inspect_ticket(let value: Ticket) -> Bool:\n    false\nfn run(own box: Box(Ticket)) -> Int:\n    match move box:\n        Box(token) if inspect_ticket(token) -> finish_ticket(token)\n        Box(token) -> finish_ticket(token)\nfn main() -> Int:\n    run(Box(Ticket(7)))\n");
        let error = source::checked(&program).expect_err("guarded extraction remains unsupported even when the guard only borrows");
        assert!(error.contains("matching would erase a must-consume obligation"), "{error}");
        let unguarded = format!("{mode}\n{WORKER}\nfn finish_ticket(own value: Ticket) -> Int:\n    match move value:\n        Ticket(n) -> n\nfn run(own box: Box(Ticket)) -> Int:\n    match move box:\n        Box(token) -> finish_ticket(token)\nfn main() -> Int:\n    run(Box(Ticket(7)))\n");
        source::checked(&unguarded).expect("unguarded extraction transfers and disposes the nested resource");
    }
}

#[test]
fn unreachable_arm_keeps_its_boundary_after_guard_disposition() {
    for mode in ["", "mode opt\n"] {
        let unreachable = DUPLICATE.replace("_ if worker.finish(token) -> 1", "_ -> if worker.finish(token): 1 else: 3")
            .replace("_ -> if worker.finish(token): 2 else: 3", "_ -> 0");
        checked(mode, mode, &unreachable).expect("a permitted redundant final wildcard cannot resurrect disposed obligations");
        let unreachable = unreachable.replace("_ -> 0", "true -> 0");
        let error = checked(mode, mode, &unreachable).expect_err("a non-wildcard unreachable arm remains invalid");
        assert!(error.contains("unreachable"), "{error}");
    }
}
