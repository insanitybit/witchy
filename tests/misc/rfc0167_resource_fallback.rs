//! Phantom resource arguments are not stored obligations in a fallback error.
//! Both branches are driven on admitted unoptimized Wasm; 7*10+9 = 79.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const FAST: &str = r#"mode opt
type Ticket:
    Ticket(Int)

must type Obligation:
    Obligation(Int)

type Phantom(a):
    Phantom(Int)

pub fn choose(flag: Bool) -> Result(Ticket, Phantom(Obligation)):
    if flag:
        Ok(Ticket(7))
    else:
        Err(Phantom(1))

pub fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(value) -> value
"#;

const MAIN: &str = r#"import fast

type Local:
    Local(Int)

must type Obligation:
    Obligation(Int)

type Phantom(a):
    Phantom(Int)

fn choose(flag: Bool) -> Result(Local, Phantom(Obligation)):
    if flag:
        Ok(Local(7))
    else:
        Err(Phantom(1))

fn finish(own value: Local) -> Int:
    match value:
        Local(n) -> n

fn local(flag: Bool) -> Int:
    let value = choose(flag) ?? Local(9)
    finish(value)

fn cross_mode(flag: Bool) -> Int:
    let value = fast.choose(flag) ?? fast.Ticket(9)
    fast.finish(value)

fn imported_choice(flag: Bool) -> Result(fast.Ticket, fast.Phantom(fast.Obligation)):
    if flag:
        Ok(fast.Ticket(7))
    else:
        Err(fast.Phantom(1))

fn imported_type(flag: Bool) -> Int:
    let value = imported_choice(flag) ?? fast.Ticket(9)
    fast.finish(value)

fn main() -> Int:
    (local(true) * 10 + local(false)) * 10000 + (cross_mode(true) * 10 + cross_mode(false)) * 100 + imported_type(true) * 10 + imported_type(false)
"#;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn fallback_discards_phantom_resource_errors_in_both_modes_and_across_imports() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for mode in ["", "mode opt\n"] {
        let main = format!("{mode}{MAIN}");
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), main.into_bytes()), ("fast".into(), FAST.as_bytes().to_vec())],
            "main",
            witchy_interp::comptime::EXPANDER,
        ).expect("phantom resource error checks");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("unoptimized canonical SafeWIR admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        ).expect("admitted Wasm instance");
        vm.run().expect("admitted Wasm execution");
        assert_eq!(vm.output(), vec!["797979"], "three independently computed 79 results");
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}
