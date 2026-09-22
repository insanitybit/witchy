//! RFC-0167 rows G/N: an `Option` shell around a view nests to any depth.
//!
//! Each shell is its own authenticated opaque owner whose `Some` field is the
//! next carrier inward. Independent scalar expectation, canonical SafeWIR
//! interpreter, admitted unoptimized Wasm, zero-leak audit on both.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_canonical_and_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)],
        "canonical interpreter matches the independent expected result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

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

/// Construction, return, and a nested refutable pattern through two shells.
#[test]
fn a_view_inside_two_option_shells_is_returned_and_matched() {
    assert_canonical_and_wasm(r#"
fn wrap(r: &Int) -> Option(Option(&Int)):
    Some(Some(r))

fn main() -> Int:
    let x = 7
    match wrap(&x):
        Some(Some(v)) -> (*v)
        Some(None) -> 1
        None -> 0
"#, 7);
}

/// Every arm is reachable: the inner `None` and the outer `None` each select
/// their own arm, and the view never materializes for either.
#[test]
fn inner_and_outer_none_select_their_own_arms() {
    assert_canonical_and_wasm(r#"
fn shape(r: &Int, depth: Int) -> Option(Option(&Int)):
    if depth == 0:
        None
    else:
        if depth == 1: Some(None) else: Some(Some(r))

fn classify(o: Option(Option(&Int))) -> Int:
    match o:
        Some(Some(v)) -> (*v)
        Some(None) -> 100
        None -> 1000

fn main() -> Int:
    let x = 5
    classify(shape(&x, 0)) + classify(shape(&x, 1)) + classify(shape(&x, 2))
"#, 1105);
}

/// Three shells deep, passed as a parameter and relayed back out.
#[test]
fn three_option_shells_relay_through_a_call() {
    assert_canonical_and_wasm(r#"
fn relay(o: Option(Option(Option(&Int)))) -> Option(Option(Option(&Int))):
    o

fn main() -> Int:
    let x = 9
    match relay(Some(Some(Some(&x)))):
        Some(Some(Some(v))) -> (*v) * 2
        _ -> 0
"#, 18);
}

/// A record may hold a direct view beside an `Option` view; each field keeps
/// its own owner through an `own` relay and both read back exactly.
#[test]
fn a_record_carries_a_direct_view_beside_an_option_view() {
    assert_canonical_and_wasm(r#"
mode opt

type Mixed:
    direct: &Int
    nested: Option(&Int)

fn relay(own mixed: Mixed) -> Mixed:
    mixed

fn main() -> Int:
    let direct = 4
    let nested = 9
    let mixed = Mixed(direct: &direct, nested: Some(&nested))
    let back = relay(move mixed)
    let inner = match back.nested:
        Some(v) -> (*v)
        None -> 0
    (*back.direct) * 10 + inner
"#, 49);
}


/// Nesting does not launder provenance: a view of a region-born root still
/// cannot leave the region inside two `Option` shells.
#[test]
fn a_region_born_root_cannot_escape_inside_nested_options() {
    let source = r#"
fn bad() -> Option(Option(&Int)):
    region -> Option(Option(&Int)):
        let values = [10, 20]
        Some(Some(&values[0]))
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a nested Option view retains its region-born root");
    assert!(error.to_string().contains("region"), "{error}");
}

/// A record field is no laundering path either: an `Option` view field over a
/// region-born root cannot leave the region inside the record.
#[test]
fn a_region_born_root_cannot_escape_in_a_record_option_field() {
    let source = r#"
mode opt

type Mixed:
    direct: &Int
    nested: Option(&Int)

fn bad(outer: &Int) -> Mixed:
    region -> Mixed:
        let values = [10, 20]
        Mixed(direct: outer, nested: Some(&values[0]))
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("the record's Option view field retains its region-born root");
    assert!(error.to_string().contains("region"), "{error}");
}

/// A direct view field authorizes only admitted carriers beside it: a field
/// with no carrier contract (`List(&Int)`) still fails closed.
#[test]
fn a_direct_view_field_cannot_admit_an_uncarried_list_of_views() {
    let source = r#"
mode opt

type Decoy:
    direct: &Int
    many: List(&Int)

fn relay(own decoy: Decoy) -> Decoy:
    decoy

fn main() -> Int:
    let a = 4
    let b = 9
    let decoy = Decoy(direct: &a, many: [&b])
    let _ = relay(move decoy)
    0
"#;
    let Ok(checked) = witchy::resolve_std_only_checked(source) else { return };
    assert!(
        !matches!(witchy::codegen::compile_checked_module_artifact(&checked),
            witchy::codegen::LoweringOutcome::Lowered(_)),
        "a List(&Int) field has no admitted carrier and must not ride on the direct view",
    );
}

/// Any non-packed shell nests a carrier, not only `Option`: a tuple inside an
/// `Option`, a `Result` inside an `Option`, an `Option` inside a tuple and a
/// `Result`, a user sum, and a record inside a record.
#[test]
fn a_tuple_view_carrier_nests_inside_an_option() {
    assert_canonical_and_wasm(r#"
fn wrap(r: &Int) -> Option((&Int, Int)):
    Some((r, 2))

fn main() -> Int:
    let x = 6
    match wrap(&x):
        Some((v, n)) -> (*v) * 10 + n
        None -> 0
"#, 62);
}

#[test]
fn a_result_view_carrier_nests_inside_an_option() {
    assert_canonical_and_wasm(r#"
fn wrap(r: &Int, ok: Bool) -> Option(Result(&Int, String)):
    if ok: Some(Ok(r)) else: Some(Err("four"))

fn score(o: Option(Result(&Int, String))) -> Int:
    match o:
        Some(Ok(v)) -> (*v)
        Some(Err(e)) -> e.length() * 100
        None -> 0

fn main() -> Int:
    let x = 3
    score(wrap(&x, true)) + score(wrap(&x, false))
"#, 403);
}

#[test]
fn an_option_view_carrier_nests_inside_a_tuple_and_a_result() {
    assert_canonical_and_wasm(r#"
fn pair(r: &Int) -> (Option(&Int), Int):
    (Some(r), 5)

fn lift(r: &Int) -> Result(Option(&Int), String):
    Ok(Some(r))

fn main() -> Int:
    let x = 3
    let (o, n) = pair(&x)
    let a = match o:
        Some(v) -> (*v) * 10 + n
        None -> 0
    let b = match lift(&x):
        Ok(Some(v)) -> (*v)
        _ -> 0
    a * 10 + b
"#, 353);
}

#[test]
fn a_record_carrier_nests_inside_a_record_and_a_user_sum() {
    assert_canonical_and_wasm(r#"
mode opt

type Inner:
    view: &Int
    bias: Int

type Outer:
    inner: Inner
    tag: Int

type Slot:
    Full(&Int)
    Empty

fn build(r: &Int) -> Outer:
    Outer(inner: Inner(view: r, bias: 2), tag: 7)

fn main() -> Int:
    let x = 5
    let o = build(&x)
    let slot = match Full(&x):
        Full(v) -> (*v)
        Empty -> 0
    ((*o.inner.view) + o.inner.bias) * 100 + o.tag * 10 + slot
"#, 775);
}

/// A generic record instantiated at a view is a carrier like any other shell.
#[test]
fn a_generic_record_instantiated_at_a_view_is_a_carrier() {
    assert_canonical_and_wasm(r#"
mode opt

type Box(a):
    value: a

type Pair:
    direct: &Int
    boxed: Box(&Int)

fn make(a: &Int, b: &Int) -> Pair:
    Pair(direct: a, boxed: Box(value: b))

fn main() -> Int:
    let x = 3
    let y = 4
    let p = make(&x, &y)
    (*p.direct) * 10 + (*p.boxed.value)
"#, 34);
}

/// A builtin shell around a view-carrying record: `Option(Inner)` chooses the
/// second view it is handed, and both arms are reachable.
#[test]
fn an_option_of_a_view_carrying_record_is_a_carrier() {
    assert_canonical_and_wasm(r#"
mode opt

type Inner:
    v: &Int

fn pick(a: &Int, b: &Int, some: Bool) -> Option(Inner):
    if some: Some(Inner(v: b)) else: None

fn read(o: Option(Inner)) -> Int:
    match o:
        Some(i) -> (*i.v)
        None -> 100

fn main() -> Int:
    let x = 1
    let y = 2
    read(pick(&x, &y, true)) + read(pick(&x, &y, false))
"#, 102);
}

/// The record inside the shell does not launder a region-born root.
#[test]
fn a_region_born_root_cannot_escape_in_an_option_of_a_record() {
    let source = r#"
mode opt

type Inner:
    v: &Int

fn bad() -> Option(Inner):
    region -> Option(Inner):
        let values = [10, 20]
        Some(Inner(v: &values[0]))
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("the record inside the Option retains its region-born root");
    assert!(error.to_string().contains("region"), "{error}");
}
