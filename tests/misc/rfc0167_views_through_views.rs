//! RFC-0167 §5: a view formed through another view.
//!
//! `&(*b).f` and `&(*v)[i]` project through a dereferenced view. The result
//! retains the original root carried by that view and composes the checked
//! projection; it still reads the formation-time snapshot after the owner
//! `var` is reassigned. Independent hand-computed expectation, admitted
//! unoptimized Wasm, zero-leak heap audit.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

/// Compile to admitted unoptimized Wasm and run it. `Ok` carries the printed
/// output after a zero-leak audit; `Err` carries the runtime failure.
fn run_admitted(source: &str) -> Result<Vec<String>, String> {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().map_err(|error| format!("{error:?}"))?;
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
    Ok(vm.output())
}

fn assert_admitted(source: &str, expected: i64) {
    let output = run_admitted(source).expect("admitted Wasm execution");
    assert_eq!(output, vec![expected.to_string()], "admitted Wasm matches the independent expectation");
}

#[test]
fn field_views_through_a_record_view_keep_the_snapshot() {
    // label "abc" -> 3 * 10000; n 7 -> 7000; tag "hello" -> 5 * 100;
    // inner.y 40 through `deep_y`, and 40 again through the `&Inner` view.
    assert_admitted(r#"
type Inner:
    y: Int
    tag: String

type Rec:
    label: String
    n: Int
    inner: Inner

fn label_of(r: &Rec) -> &String:
    &(*r).label

fn count_of(r: &Rec) -> &Int:
    &(*r).n

fn inner_of(r: &Rec) -> &Inner:
    &(*r).inner

fn deep_y(r: &Rec) -> &Int:
    &(*r).inner.y

fn main() -> Int:
    var r = Rec(label: "abc", n: 7, inner: Inner(y: 40, tag: "hello"))
    let b = &r
    let label = label_of(b)
    let n = count_of(b)
    let inner = inner_of(b)
    let y = deep_y(&r)
    let tag = &(*inner).tag
    r = Rec(label: "zz", n: 1, inner: Inner(y: 2, tag: "q"))
    (*label).length() * 10000 + *n * 1000 + (*tag).length() * 100 + *y + (*inner).y
"#, 37580);
}

#[test]
fn element_views_through_a_list_view_keep_the_snapshot() {
    // second -> 20; at(3) -> 40 * 10; middle [20, 30]: length 2 * 1000 and
    // element 1 = 30 * 10000; word(1) "bbb" -> 3 * 1000000.
    assert_admitted(r#"
fn second(v: &List(Int)) -> &Int:
    &(*v)[1]

fn at(v: &List(Int), i: Int) -> &Int:
    &(*v)[i]

fn middle(v: &List(Int)) -> &[Int]:
    &(*v)[1..3]

fn word(v: &List(String), i: Int) -> &String:
    &(*v)[i]

fn main() -> Int:
    var xs = [10, 20, 30, 40]
    var ws = ["a", "bbb", "cc"]
    let s = second(&xs)
    let d = at(&xs, 3)
    let m = middle(&xs)
    let w = word(&ws, 1)
    xs = [0]
    ws = []
    *s + *d * 10 + m.length() * 1000 + m[1] * 10000 + (*w).length() * 1000000
"#, 3302420);
}

#[test]
fn element_views_of_record_fields_compose_through_a_view() {
    // local `&bag.items[2]` -> 7 * 100; pick(1) through the view -> 6 * 10;
    // names[0] "xy" -> 2.
    assert_admitted(r#"
type Bag:
    items: List(Int)
    names: List(String)

fn pick(b: &Bag, i: Int) -> &Int:
    &(*b).items[i]

fn first_name(b: &Bag) -> &String:
    &(*b).names[0]

fn main() -> Int:
    var bag = Bag(items: [5, 6, 7], names: ["xy", "z"])
    let local = &bag.items[2]
    let p = pick(&bag, 1)
    let name = first_name(&bag)
    bag = Bag(items: [], names: [])
    *local * 100 + *p * 10 + (*name).length()
"#, 762);
}

#[test]
fn element_view_through_a_view_is_bounds_checked_when_formed() {
    let failure = run_admitted(r#"
fn at(v: &List(Int), i: Int) -> &Int:
    &(*v)[i]

fn main() -> Int:
    let xs = [1, 2, 3]
    let view = at(&xs, 3)
    0
"#).expect_err("an out-of-bounds element view traps at formation");
    assert!(!failure.is_empty());
}
