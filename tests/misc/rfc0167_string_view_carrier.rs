//! RFC-0167 rows G/N: a whole immutable `&String` view is a rooted carrier.
//!
//! The carrier is the retained String object itself (`StringRoot`): a view
//! of a whole immutable String has no projection, so retaining the root is
//! the value-semantic snapshot. Independent Int expectation, canonical
//! SafeWIR interpreter, admitted unoptimized Wasm, zero-leak audit on both.
//! The source interpreter is not consulted.

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
fn string_view_relays_through_a_function_and_reads_its_length() {
    assert_canonical_and_wasm(
        "fn keep(s: &String) -> &String:\n    s\n\nfn main() -> Int:\n    let s = \"hello\"\n    let v = keep(&s)\n    (*v).length()\n",
        5,
    );
}

#[test]
fn string_view_of_a_callee_local_is_returned_alive() {
    assert_canonical_and_wasm(
        "fn make() -> &String:\n    let local = \"abcdefg\"\n    &local\n\nfn main() -> Int:\n    let v = make()\n    (*v).length() * 10 + (*v).length()\n",
        77,
    );
}

#[test]
fn string_view_is_a_snapshot_across_owner_reassignment() {
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var s = \"abc\"\n    let v = &s\n    s = \"wxyz12\"\n    (*v).length() * 10 + s.length()\n",
        36,
    );
}

#[test]
fn string_view_passes_through_a_generic_identity() {
    assert_canonical_and_wasm(
        "fn id[a](x: &a) -> &a:\n    x\n\nfn read(v: &String) -> Int:\n    (*v).length()\n\nfn main() -> Int:\n    let s = \"four\"\n    let v = id(&s)\n    read(v) + (*id(v)).length()\n",
        8,
    );
}

#[test]
fn region_born_string_root_cannot_escape_through_a_view() {
    let source = "fn bad() -> &String:\n    region:\n        let local = \"region-owned\"\n        let view = &local\n        view\n\nfn main() -> Int:\n    (*bad()).length()\n";
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a region-born String root must not escape through &String");
    assert!(error.to_string().contains("region"), "the rejection names the region escape: {error}");
}

#[test]
fn float_view_still_fails_closed_at_the_canonical_boundary() {
    let source = "fn relay(view: &Float) -> &Float:\n    view\n\nfn main() -> Int:\n    let value = 7.0\n    let observed = relay(&value)\n    0\n";
    let checked = witchy::resolve_std_only_checked(source)
        .expect("unadmitted payload still type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("&Float has no admitted carrier");
    assert!(
        refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"),
        "the boundary must reject the unauthenticated carrier: {refusal}",
    );
}

/// A heap-built String (not a static literal) is retained by its view token:
/// growing the owner in a loop leaves the view on the original value.
#[test]
fn a_view_of_a_heap_string_keeps_its_snapshot_while_the_owner_grows() {
    assert_canonical_and_wasm(r#"
fn main() -> Int:
    var s = "ab" + "c"
    let v = &s
    for i in 0..5:
        s = s + "x"
    (*v).length() * 10 + s.length()
"#, 38);
}

/// A `var` parameter write-back of a heap String leaves the caller's earlier
/// view on the value it was formed over.
#[test]
fn a_var_write_back_does_not_change_a_held_heap_string_view() {
    assert_canonical_and_wasm(r#"
fn grow(var s: String):
    s = s + "yz"

fn main() -> Int:
    var s = "a" + "bc"
    let v = &s
    grow(s)
    (*v).length() * 10 + s.length()
"#, 35);
}

/// A closure may capture a `&String`: the capture is the retained String
/// token, so the closure reads the value the view was formed over.
#[test]
fn a_closure_captures_a_string_view() {
    assert_canonical_and_wasm(r#"
fn main() -> Int:
    let s = "hello"
    let v = &s
    let f = fn(k: Int) -> Int: (*v).length() + k
    f(10)
"#, 15);
}

/// The captured view keeps its snapshot after the owner grows, read through
/// an indirect call.
#[test]
fn a_captured_string_view_keeps_its_snapshot_through_an_indirect_call() {
    assert_canonical_and_wasm(r#"
fn apply(f: fn(Int) -> Int, k: Int) -> Int:
    f(k)

fn main() -> Int:
    var s = "ab" + "c"
    let v = &s
    let f = fn(k: Int) -> Int: (*v).length() * k
    s = s + "zzzz"
    apply(f, 10) + s.length()
"#, 37);
}

/// Inside an aggregate a `&String` is its retained String token, so records,
/// tuples, and `Option`s of `&String` cross calls with the String layout.
#[test]
fn a_record_holds_a_string_view() {
    assert_canonical_and_wasm(r#"
type Holder:
    v: &String
    n: Int

fn main() -> Int:
    let s = "hello"
    let h = Holder(v: &s, n: 2)
    (*h.v).length() * 10 + h.n
"#, 52);
}

#[test]
fn tuple_and_option_string_views_cross_calls() {
    assert_canonical_and_wasm(r#"
fn pair(s: &String) -> (&String, Int):
    (s, 3)

fn pick(s: &String) -> Option(&String):
    Some(s)

fn main() -> Int:
    let s = "abc"
    let (v, n) = pair(&s)
    let m = match pick(&s):
        Some(w) -> (*w).length()
        None -> 0
    (*v).length() * 100 + n * 10 + m
"#, 333);
}

#[test]
fn an_aggregate_string_view_keeps_its_snapshot_across_a_call() {
    assert_canonical_and_wasm(r#"
mode opt

type Holder:
    v: &String
    n: Int

fn wrap(s: &String, n: Int) -> Holder:
    Holder(v: s, n: n)

fn main() -> Int:
    var s = "ab" + "c"
    let h = wrap(&s, 4)
    s = s + "zzzz"
    (*h.v).length() * 100 + h.n * 10 + s.length()
"#, 347);
}

#[test]
fn a_region_born_string_cannot_escape_inside_an_option_view() {
    let source = r#"
fn bad() -> Option(&String):
    region -> Option(&String):
        let local = "region-owned"
        Some(&local)
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a region-born String root must not escape inside an aggregate view");
    assert!(error.to_string().contains("region"), "{error}");
}
