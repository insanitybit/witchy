//! RFC-0167 row G: an exact `&[Int]` slice carried in a nominal record field.
//!
//! The expected 202 is hand-computed from `[1, 2, 3, 4][1..3]`: its first
//! projected element is 2 and its length is 2.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn nominal_record_field_carries_an_exact_int_slice() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
type Packet:
    data: &[Int]

fn score(packet: Packet) -> Int:
    packet.data[0] * 100 + packet.data.length()

fn main() -> Int:
    let xs = [1, 2, 3, 4]
    let packet = Packet(data: &xs[1..3])
    score(packet)
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("canonical SafeWIR admission");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(vm.output(), vec!["202"]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

#[test]
fn region_born_slice_cannot_escape_through_a_record_field() {
    let source = r#"
type Packet:
    data: &[Int]

fn bad() -> Packet:
    region -> Packet:
        let local = [1, 2, 3]
        Packet(data: &local[1..3])
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a region-born slice root must not escape in a record field");
    assert!(error.to_string().contains(
        "cannot copy out a region aggregate carrying immutable views without per-field parent-root provenance"
    ), "{error}");
}
