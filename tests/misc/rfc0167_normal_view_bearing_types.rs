//! RFC-0167 §5-§6: nominal types whose fields carry immutable rooted views are
//! ordinary values in BOTH modes. A normal module may declare and name one,
//! and may import one exported by an opt module (`import` or `from ... import`)
//! and pass it across the mode boundary in both directions.
//!
//! Only types reaching a retired non-rooted reference (lifetime / `&mut`
//! carriers, spellable only by the migration grammar) stay closed to normal
//! importers; `witchy-syntax`'s linker and type_resolve unit tests pin that
//! refusal. A `var` parameter of a view-bearing record also has a checked
//! write-back lane through canonical lowering.
//!
//! Independent hand-computed expectation, admitted unoptimized Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_admitted_wasm(checked: &witchy_types::pipeline::CheckedModule, expected: i64) {
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

#[test]
fn a_normal_module_declares_and_rebinds_a_record_holding_a_view() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // before = 1 (views `a`); `rebuilt` returns a record viewing `b` = 2.
    let checked = witchy::resolve_std_only_checked(r#"
type H:
    v: &Int

fn rebuilt(h: H, other: &Int) -> H:
    if *h.v > 0: H(v: other) else: h

fn main() -> Int:
    let a = 1
    let b = 2
    var h = H(v: &a)
    let before = *h.v
    h = rebuilt(h, &b)
    before * 10 + *h.v
"#)
    .expect("a normal module names its own view-bearing record");
    assert_admitted_wasm(&checked, 12);
}

#[test]
fn a_normal_var_parameter_writes_back_a_view_bearing_record() {
    // `bump` replaces the caller's record with one viewing `b`; `a` itself is
    // untouched. (*h.v) * 10 + a = 2 * 10 + 1.
    let checked = witchy::resolve_std_only_checked(r#"
type H:
    v: &Int

fn bump(var h: H, other: &Int):
    h = H(v: other)

fn main() -> Int:
    let a = 1
    let b = 2
    var h = H(v: &a)
    bump(h, &b)
    (*h.v) * 10 + a
"#)
    .expect("a normal `var` parameter may name a view-bearing record");
    assert_admitted_wasm(&checked, 21);
}

#[test]
fn an_indirect_var_parameter_writes_back_a_view_bearing_record() {
    let checked = witchy::resolve_std_only_checked(r#"
type H:
    v: &Int

fn bump(var h: H, other: &Int):
    h = H(v: other)

fn main() -> Int:
    let a = 1
    let b = 2
    var h = H(v: &a)
    let before = h
    let change = bump
    change(h, &b)
    (*h.v) * 100 + (*before.v) * 10 + a
"#)
    .expect("indirect `var` may name a view-bearing record");
    assert_admitted_wasm(&checked, 211);
}

#[test]
fn an_indirect_var_parameter_writes_back_a_nested_view_record() {
    let checked = witchy::resolve_std_only_checked(r#"
type H:
    v: Option(&Int)

fn bump(var h: H, other: &Int):
    h = H(v: Some(other))

fn read(h: H) -> Int:
    match h.v:
        Some(item) -> *item
        None -> 0

fn main() -> Int:
    let a = 1
    let b = 2
    var h = H(v: Some(&a))
    let before = h
    let change = bump
    change(h, &b)
    read(h) * 100 + read(before) * 10 + a
"#)
    .expect("indirect `var` may name a record holding a nested view");
    assert_admitted_wasm(&checked, 211);
}

#[test]
fn a_generic_var_parameter_writes_back_a_view_carrier() {
    let checked = witchy::resolve_std_only_checked(r#"
type Box(a):
    value: a

fn replace[a](var box: Box(a), other: a):
    box = Box(value: other)

fn main() -> Int:
    let first = 1
    let second = 2
    var box = Box(value: &first)
    let before = box
    let change = replace
    change(box, &second)
    (*box.value) * 100 + (*before.value) * 10 + first
"#)
    .expect("generic var write-back keeps a concrete view carrier");
    assert_admitted_wasm(&checked, 211);
}

#[test]
fn generic_var_view_write_back_cannot_escape_a_region() {
    let source = r#"
type Box(a):
    value: a

fn replace[a](var box: Box(a), other: a):
    box = Box(value: other)

fn main() -> Int:
    let root = 1
    var box = Box(value: &root)
    let change = replace
    let _ = region -> ():
        let local = 2
        change(box, &local)
    0
"#;
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .map(|error| error.to_string())
        .expect("region-born root cannot leave through generic var write-back");
    assert!(error.contains("cannot write back into `box` inside `region:`")
        && error.contains("can hold a view"), "refusal names the region write-back rule: {error}");
}

const FAST: &str = r#"mode opt

type Holder:
    value: &Int

pub fn make(value: &Int) -> Holder:
    Holder(value: value)

pub fn read(let h: Holder) -> Int:
    *h.value

pub fn swap(let h: Holder, other: &Int) -> Holder:
    Holder(value: other)
"#;

fn checked_graph(main: &str) -> witchy_types::pipeline::CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("fast".into(), FAST.as_bytes().to_vec()),
            ("main".into(), main.as_bytes().to_vec()),
        ],
        "main",
        witchy_interp::comptime::EXPANDER,
    )
    .expect("a normal module imports an opt module's view-bearing record")
}

#[test]
fn a_normal_module_imports_and_round_trips_an_opt_view_bearing_record() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // direct = 3 (normal-constructed, read in normal code),
    // made   = 5 (opt-constructed, read by opt), swapped = 5 (normal -> opt -> opt read),
    // kept   = 3 (normal-constructed, passed through a normal fn, read in normal code).
    let checked = checked_graph(r#"import fast

fn keep(h: fast.Holder) -> fast.Holder:
    h

fn main() -> Int:
    let a = 3
    let b = 5
    let h = fast.Holder(value: &a)
    let direct = *h.value
    let made = fast.make(&b)
    let swapped = fast.swap(h, &b)
    let kept = keep(h)
    direct * 1000 + fast.read(made) * 100 + fast.read(swapped) * 10 + *kept.value
"#);
    assert_admitted_wasm(&checked, 3553);
}

#[test]
fn a_normal_module_from_imports_an_opt_view_bearing_record() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // The opt-built record views `b` (7); normal code rebinds it over `a` (4).
    let checked = checked_graph(r#"from fast import Holder, make

fn main() -> Int:
    let a = 4
    let b = 7
    var h: Holder = make(&b)
    let first = *h.value
    h = Holder(value: &a)
    first * 10 + *h.value
"#);
    assert_admitted_wasm(&checked, 74);
}

#[test]
fn normal_indirect_var_rebinds_an_opt_defined_view_record() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = checked_graph(r#"from fast import Holder, make

fn retarget(var h: Holder, other: &Int):
    h = Holder(value: other)

fn main() -> Int:
    let a = 4
    let b = 7
    var h: Holder = make(&a)
    let before = h
    let change = retarget
    change(h, &b)
    *h.value * 100 + *before.value * 10 + a
"#);
    assert_admitted_wasm(&checked, 744);
}
