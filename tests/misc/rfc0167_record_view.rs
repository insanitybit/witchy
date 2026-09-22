//! RFC-0167 row G: a view of a whole user record or sum.
//!
//! `&p` is carried as the retained record object: a record is an immutable
//! value shared by retain, so the token is the snapshot (`RecordRoot`).
//! Independent expectation, canonical SafeWIR interpreter, admitted Wasm, zero leaks.

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

#[test]
fn a_record_view_crosses_a_call_and_reads_its_fields() {
    assert_canonical_and_wasm(r#"
type Point:
    x: Int
    y: Int

fn keep(p: &Point) -> &Point:
    p

fn main() -> Int:
    let p = Point(x: 3, y: 4)
    let v = keep(&p)
    (*v).x * 10 + (*v).y
"#, 34);
}

#[test]
fn a_record_view_keeps_its_snapshot_when_the_owner_is_reassigned() {
    assert_canonical_and_wasm(r#"
type Point:
    x: Int
    y: Int

fn main() -> Int:
    var p = Point(x: 3, y: 4)
    let v = &p
    p = Point(x: 7, y: 9)
    (*v).x * 1000 + (*v).y * 100 + p.x * 10 + p.y
"#, 3479);
}

#[test]
fn a_sum_view_is_matched_through_a_read() {
    assert_canonical_and_wasm(r#"
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: &Shape) -> Int:
    match (*s):
        Circle(r) -> r * 3
        Square(n) -> n * n

fn main() -> Int:
    let a = Circle(2)
    let b = Square(5)
    area(&a) * 100 + area(&b)
"#, 625);
}

#[test]
fn a_record_view_with_a_string_field_passes_through_a_generic() {
    assert_canonical_and_wasm(r#"
type Named:
    label: String
    size: Int

fn id[a](x: a) -> a:
    x

fn main() -> Int:
    let n = Named(label: "abc", size: 4)
    let v = id(&n)
    (*v).label.length() * 10 + (*v).size
"#, 34);
}

#[test]
fn a_region_born_record_cannot_escape_through_its_view() {
    let source = r#"
type Point:
    x: Int
    y: Int

fn bad() -> &Point:
    region -> &Point:
        let local = Point(x: 1, y: 2)
        &local
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a region-born record root must not escape through its view");
    assert!(error.to_string().contains("region"), "{error}");
}

/// A view of a String field is that field's retained token.
#[test]
fn a_view_of_a_string_field_reads_that_field() {
    assert_canonical_and_wasm(r#"
type Named:
    label: String
    size: Int

fn main() -> Int:
    let n = Named(label: "abcd", size: 2)
    let v = &n.label
    (*v).length() * 10 + n.size
"#, 42);
}

/// Views of a record field and a String field keep the values they were
/// formed over after the owning record is reassigned, including through a call.
#[test]
fn record_and_string_field_views_keep_their_snapshots() {
    assert_canonical_and_wasm(r#"
type In:
    x: Int
    y: Int

type Out:
    inner: In
    tag: String

fn keep(r: &In) -> &In:
    r

fn main() -> Int:
    var o = Out(inner: In(x: 3, y: 4), tag: "t")
    let v = keep(&o.inner)
    let w = &o.tag
    o = Out(inner: In(x: 8, y: 9), tag: "zz")
    (*v).x * 1000 + (*v).y * 100 + (*w).length() * 10 + o.tag.length()
"#, 3412);
}

/// A generic record is a record root per instantiation: `&Box(Int)` and
/// `&Box(String)` in one program each read their own payload.
#[test]
fn views_of_two_instantiations_of_one_generic_record() {
    assert_canonical_and_wasm(r#"
type Box(a):
    value: a

fn keep_int(b: &Box(Int)) -> &Box(Int):
    b

fn keep_text(b: &Box(String)) -> &Box(String):
    b

fn main() -> Int:
    let n = Box(value: 7)
    let t = Box(value: "four")
    (*keep_int(&n)).value * 10 + (*keep_text(&t)).value.length()
"#, 74);
}

/// Builtin `Option` and `Result` values are record roots too.
#[test]
fn views_of_option_and_result_values_match_through_a_read() {
    assert_canonical_and_wasm(r#"
fn opt(o: &Option(Int)) -> Int:
    match (*o):
        Some(n) -> n
        None -> 0

fn res(r: &Result(Int, String)) -> Int:
    match (*r):
        Ok(n) -> n
        Err(e) -> e.length() * 100

fn main() -> Int:
    let a = Some(9)
    let b: Result(Int, String) = Ok(4)
    let c: Result(Int, String) = Err("xyz")
    opt(&a) * 1000 + res(&b) * 10 + res(&c)
"#, 9340);
}

#[test]
fn a_generic_record_view_keeps_its_snapshot() {
    assert_canonical_and_wasm(r#"
type Box(a):
    value: a

fn main() -> Int:
    var b = Box(value: 3)
    let v = &b
    b = Box(value: 8)
    (*v).value * 10 + b.value
"#, 38);
}

/// The ABI admits a record-root view by the same rule lowering and authority
/// apply, so a packed (inline) record's view has no record-root carrier and
/// is refused at the boundary rather than mis-carried.
#[test]
fn a_packed_record_view_has_no_record_root_carrier() {
    let source = r#"
type Point packed:
    x: Int
    y: Int

fn keep(p: &Point) -> &Point:
    p

fn main() -> Int:
    let p = Point(x: 1, y: 2)
    (*keep(&p)).x
"#;
    let Ok(checked) = witchy::resolve_std_only_checked(source) else { return };
    assert!(
        !matches!(witchy::codegen::compile_checked_module_artifact(&checked),
            witchy::codegen::LoweringOutcome::Lowered(_)),
        "a packed record has no record-root view carrier",
    );
}
