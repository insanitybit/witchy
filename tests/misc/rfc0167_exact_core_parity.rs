//! RFC-0167 exact-tip core conformance matrix.
//!
//! Each row has an independent expected observable result. The canonical
//! SafeWIR interpreter and admitted `OptSet::none()` Wasm each answer that
//! expectation separately; backend agreement is not the oracle.
//!
//! Rooted-view aggregate, callable, and projection rows live in their own
//! acceptance matrices. This file covers the non-carrier core contract.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[derive(Clone, Copy)]
enum Program {
    Source(&'static str),
    Bundle {
        entry: &'static str,
        modules: &'static [(&'static str, &'static str)],
    },
}

struct Case {
    row: &'static str,
    name: &'static str,
    program: Program,
    expected: &'static [&'static str],
}

const UNIT_RESULT: &str = r#"
pub fn explicit() -> ():
    ()

pub fn omitted():
    ()

fn main(console: Console):
    explicit()
    omitted()
    console.print("unit")
"#;

const OWN_TRANSFER: &str = r#"
fn consume(own text: String) -> String:
    text

fn main(console: Console):
    let original = "hello"
    let snapshot = original
    let result = consume(original)
    console.print("${snapshot}:${result}")
"#;

const UNIQUE_WRITEBACK: &str = r#"mode opt

fn update(var values: unique List(Int)):
    values[0] = 9

fn main(console: Console):
    var values: unique List(Int) = [1, 2]
    update(values)
    console.print("${values[0]}")
"#;

const FROZEN_WRITEBACK: &str = r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let replacement = replace(values)
    console.print("${values[0]}:${replacement[0]}")
"#;

const OPT_DIRECT_WORKER: &str = r#"mode opt

pub fn append(own values: List(Int), value: Int) -> List(Int):
    values.push(value)
    values
"#;

const OPT_DIRECT_API: &str = r#"mode opt

import worker

pub fn extend(own values: List(Int), value: Int) -> List(Int):
    worker.append(values, value)
"#;

const OPT_DIRECT_MAIN: &str = r#"import api

fn main(console: Console):
    let values = [1, 2]
    let before = values
    let extended = api.extend(values, 3)
    console.print("${extended}:${before}")
"#;

const OPT_INDIRECT_WORKER: &str = r#"mode opt

type Packet:
    values: List(Int)

trait Advance:
    fn advance(var self: unique Packet, value: Int)

impl Advance for Packet:
    fn advance(var self: unique Packet, value: Int):
        self = Packet(values: [value])

pub fn retain(own value: unique a) -> unique a:
    value
"#;

const OPT_INDIRECT_API: &str = r#"mode opt

import worker

pub fn relay(own packet: unique worker.Packet, value: Int) -> unique worker.Packet:
    let keep = worker.retain
    var current = keep(packet)
    current.advance(value)
    current
"#;

const OPT_INDIRECT_MAIN: &str = r#"import api
import worker

fn main(console: Console):
    var packet = worker.Packet(values: [1, 2])
    let before = packet
    packet = api.relay(packet, 3)
    console.print("${packet.values}:${before.values}")
"#;

const RESOURCE_COMPOSITION: &str = r#"mode opt

must sealed type Completion:
    Completion(pure once fn(Int) -> Int)

fn finish(own completion: Completion, value: Int) -> Int:
    match completion:
        Completion(callback) -> callback(value)

fn main(console: Console):
    let completion = Completion(pure once fn(value: Int): value + 1)
    console.print("${finish(completion, 41)}")
"#;

const REGION_PROMOTION: &str = r#"mode opt

fn main(console: Console):
    let parent = "parent-owned"
    let value: String = region -> String:
        parent
    console.print(value)
"#;

const CASES: &[Case] = &[
    Case { row: "C", name: "explicit and omitted unit results share the unit convention", program: Program::Source(UNIT_RESULT), expected: &["unit"] },
    Case { row: "D", name: "own transfer preserves the caller snapshot", program: Program::Source(OWN_TRANSFER), expected: &["hello:hello"] },
    Case { row: "E", name: "opt unique write-back updates the proven owner", program: Program::Source(UNIQUE_WRITEBACK), expected: &["9"] },
    Case { row: "F", name: "frozen write-back replaces rather than deep-mutates storage", program: Program::Source(FROZEN_WRITEBACK), expected: &["2:2"] },
    Case {
        row: "I/J",
        name: "normal caller reaches opt direct owned chain",
        program: Program::Bundle {
            entry: "main",
            modules: &[("worker", OPT_DIRECT_WORKER), ("api", OPT_DIRECT_API), ("main", OPT_DIRECT_MAIN)],
        },
        expected: &["[1, 2, 3]:[1, 2]"],
    },
    Case {
        row: "I/J",
        name: "normal caller reaches opt indirect generic and trait chain",
        program: Program::Bundle {
            entry: "main",
            modules: &[("worker", OPT_INDIRECT_WORKER), ("api", OPT_INDIRECT_API), ("main", OPT_INDIRECT_MAIN)],
        },
        expected: &["[3]:[1, 2]"],
    },
    Case { row: "K", name: "must pure-once resource composition consumes exactly once", program: Program::Source(RESOURCE_COMPOSITION), expected: &["42"] },
    Case { row: "L", name: "parent-owned opt region promotion preserves its value", program: Program::Source(REGION_PROMOTION), expected: &["parent-owned"] },
    // Pending extension points: projected-return; function-value; generic;
    // aggregate and Result rooted-view carriers.
];

fn checked(case: &Case) -> witchy_types::pipeline::CheckedModule {
    match case.program {
        Program::Source(source) => witchy::resolve_std_only_checked(source)
            .unwrap_or_else(|error| panic!("{}: source checks: {error}", case.name)),
        Program::Bundle { entry, modules } => witchy_types::pipeline::link_checked_from_source_bundle(
            modules.iter().map(|(name, source)| ((*name).to_owned(), source.as_bytes().to_vec())).collect(),
            entry,
            witchy_interp::comptime::expand_compile_time,
        )
        .unwrap_or_else(|error| panic!("{}: linked source checks: {error}", case.name)),
    }
}

fn expected(case: &Case) -> Vec<String> {
    case.expected.iter().map(|line| (*line).to_owned()).collect()
}

fn assert_case(case: &Case) {
    let checked = checked(case);
    let expected = expected(case);
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("row {} {}: canonical lowering", case.row, case.name));
    let entry = artifact.entry().expect("canonical entry");
    let function = FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("canonical SafeWIR validation");
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, Default::default())
        .unwrap_or_else(|error| panic!("{}: canonical interpreter: {error}", case.name));
    assert_eq!(oracle.output, expected, "row {} {}: independent canonical result", case.row, case.name);
    assert_eq!(oracle.leaked_objects, 0, "row {} {}: canonical interpreter heap audit", case.row, case.name);
    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        )
        .unwrap_or_else(|error| panic!("{}: admitted Wasm spawn: {error}", case.name));
    vm.run().unwrap_or_else(|error| panic!("{}: admitted Wasm execution: {error}", case.name));
    assert_eq!(vm.output(), expected, "row {} {}: admitted Wasm result", case.row, case.name);
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "row {} {}: admitted Wasm exact heap audit",
        case.row,
        case.name,
    );
}

#[test]
fn exact_tip_admitted_core_rows_match_independent_canonical_and_wasm_results() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(opt::active_schema_key().enabled_mask(), 0, "this matrix is the no-optional-optimization canonical Wasm baseline");
    for case in CASES {
        assert_case(case);
    }
}
