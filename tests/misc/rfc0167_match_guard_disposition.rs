//! Match decisions retain ordered guard effects and resource disposition.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const FAST: &str = r#"mode opt
must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
pub fn finish(own value: Box(Ticket)) -> Bool:
    match move value:
        Box(ticket) -> finish_ticket(ticket)
fn finish_ticket(own ticket: Ticket) -> Bool:
    match move ticket:
        Ticket(n) -> n == 0
"#;

const MAIN: &str = r#"import fast
must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
fn finish(own value: Box(Ticket)) -> Bool:
    match move value:
        Box(ticket) -> finish_ticket(ticket)
fn finish_ticket(own ticket: Ticket) -> Bool:
    match move ticket:
        Ticket(n) -> n == 0
fn imported_finish(own value: fast.Box(fast.Ticket)) -> Bool:
    match move value:
        fast.Box(ticket) -> imported_finish_ticket(ticket)
fn imported_finish_ticket(own ticket: fast.Ticket) -> Bool:
    match move ticket:
        fast.Ticket(n) -> n == 0
fn local(n: Int) -> Int:
    let token = Box(Ticket(n))
    match true:
        _ if finish(token) -> 1
        _ -> 3
fn cross(n: Int) -> Int:
    let token = fast.Box(fast.Ticket(n))
    match true:
        _ if fast.finish(token) -> 1
        _ -> 3
fn imported(n: Int) -> Int:
    let token = fast.Box(fast.Ticket(n))
    match true:
        _ if imported_finish(token) -> 1
        _ -> 3
fn main() -> Int:
    (local(0) * 10 + local(7)) * 10000 + (cross(0) * 10 + cross(7)) * 100 + imported(0) * 10 + imported(7)
"#;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn guard_disposition_survives_both_outcomes_and_mode_boundaries() {
    assert_execution(MAIN, "131313");
}

fn assert_execution(main: &str, expected: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for mode in ["", "mode opt\n"] {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), format!("{mode}{main}").into_bytes()), ("fast".into(), FAST.as_bytes().to_vec())],
            "main", witchy_interp::comptime::EXPANDER,
        ).expect("checked guard disposition");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("unoptimized guard disposition admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("guard disposition execution");
        assert_eq!(vm.output(), vec![expected]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn refutable_guards_preserve_order_and_postguard_disposition() {
    assert_execution(r#"import fast
fn select(p: (Int, Int)) -> Int:
    match p:
        (x, 2) | (1, x) -> x
        _ -> 9
fn guarded(p: (Int, Int)) -> Int:
    match p:
        (x, 2) | (1, x) if x == 2 -> x
        _ -> 9
fn after(n: Int) -> Int:
    let token = fast.Box(fast.Ticket(n))
    let chosen = match n:
        0 | 7 if n < 0 -> 4
        _ -> 5
    let disposed = if fast.finish(token): 1 else: 3
    chosen * 10 + disposed
fn looping() -> Int:
    var round = 0
    var total = 0
    while round < 2:
        var index = 0
        while index < 3:
            let value = match index:
                0 | 1 if index == 1 -> 7
                _ -> 2
            total = total + value
            index = index + 1
        round = round + 1
    total
fn main() -> Int:
    let ordering = select((1, 2)) + select((1, 3)) * 10 + guarded((1, 2)) * 100 + guarded((2, 2)) * 1000
    ordering * 1000 + after(0) + after(7) + after(9) + looping()
"#, "2931179");
}

#[test]
fn false_guard_writeback_reaches_later_arm_and_match_continuation() {
    assert_execution(r#"fn bump(var n: Int) -> Bool:
    n = n + 1
    false
fn pick(code: Int) -> Int:
    var n = 0
    let chosen = match code:
        1 | 2 if bump(n) -> 7
        _ -> n
    chosen * 10 + n
fn append(var values: String) -> Bool:
    values = values + "!"
    false
fn pick_reference(code: Int) -> Int:
    var values = "x"
    let chosen = match code:
        1 | 2 if append(values) -> 7
        _ -> values.length()
    chosen * 10 + values.length()
fn main() -> Int:
    (pick(1) * 100 + pick(0)) * 10000 + pick_reference(1) * 100 + pick_reference(0)
"#, "11002211");
}
