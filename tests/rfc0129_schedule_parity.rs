//! RFC-0129 acceptance row 3: the interpreter and compiled Wasm execute one
//! complete deterministic task/channel schedule with identical exact output.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const SCHEDULE: &str = include_str!("fixtures/rfc0129_deterministic_schedule.witchy");

const EXPECTED: [&str; 13] = [
    "bounded Some(10) Some(20) Some(30)",
    "quiescent join released",
    "quiescent child done",
    "select first 1",
    "select second 2",
    "select closed",
    "cancel done",
    "quiescence None Some(42)",
    "join a",
    "join b",
    "scope done",
    "packets 200 40000",
    "text witchy",
];

#[test]
fn rfc0129_acceptance_row_3_deterministic_schedule_backends_agree() {
    let checked = witchy::resolve_std_only_checked(SCHEDULE)
        .expect("RFC-0129 row-3 schedule must check");

    // Keep compiled Wasm authoritative while the direct-carrier ABI settles.
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0129 row-3 schedule");
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-3 runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0129 row-3 compiled Wasm");
    actor.run().expect("run RFC-0129 row-3 compiled Wasm");
    assert_eq!(actor.output(), EXPECTED, "compiled Wasm schedule changed");

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("run RFC-0129 row-3 schedule on the interpreter");
    assert_eq!(interpreted, EXPECTED, "interpreter schedule changed");
    assert_eq!(interpreted, actor.output(), "row-3 backends diverged");
}
