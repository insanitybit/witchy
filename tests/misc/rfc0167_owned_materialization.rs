//! RFC-0167 §5 row G: `.owned()` explicitly materializes an independent owned
//! value. It is compiler-known (no import): on a view `&T` it reads the
//! referent exactly as `*view` does, and on an owned copyable value it is
//! identity. The result carries no view, so later mutation of either the root
//! or the copy is invisible to the other. Authority, `must` payloads, slices,
//! and view-bearing aggregates are refused while checking.
//! Independent expectation, admitted unoptimized Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_wasm(source: &str, expected: i64) {
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
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

fn assert_refused(source: &str, diagnostic: &str) {
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .expect("`.owned()` is refused while checking")
        .to_string();
    assert!(error.contains(diagnostic), "{error}");
}

#[test]
fn an_int_view_materializes_a_snapshot() {
    // copy = 4 (the value at `&n`), then n = 50: 4 * 100 + 50.
    assert_wasm(r#"
fn main() -> Int:
    var n = 4
    let v = &n
    let copy: Int = v.owned()
    n = 50
    copy * 100 + n
"#, 450);
}

#[test]
fn a_string_view_materializes_an_independent_string() {
    // copy "abc" -> "abcde" (5); root "z" (1); the view still sees "abc" (3).
    assert_wasm(r#"
fn main() -> Int:
    var s = "abc"
    let v = &s
    var copy: String = v.owned()
    s = "z"
    copy = copy + "de"
    copy.length() * 100 + s.length() * 10 + (*v).length()
"#, 513);
}

#[test]
fn a_list_view_materializes_an_independent_list() {
    // copy [1,2,3,500,6000] (5), root [1,2,3,40] (4), view [1,2,3] (3),
    // copy[3] / 100 = 5: 5000 + 400 + 30 + 5.
    assert_wasm(r#"
fn main() -> Int:
    var xs = [1, 2, 3]
    let v = &xs
    var copy: List(Int) = v.owned()
    xs.push(40)
    copy.push(500)
    copy.push(6000)
    copy.length() * 1000 + xs.length() * 100 + (*v).length() * 10 + copy[3] / 100
"#, 5435);
}

#[test]
fn a_record_view_materializes_an_independent_record() {
    // copy (3,4) -> (13,4); root (7,9); the view still sees x = 3.
    assert_wasm(r#"
type Point:
    x: Int
    y: Int

fn main() -> Int:
    var p = Point(x: 3, y: 4)
    let v = &p
    var copy: Point = v.owned()
    p = Point(x: 7, y: 9)
    copy = Point(x: copy.x + 10, ..copy)
    copy.x * 1000 + copy.y * 100 + p.x * 10 + (*v).x
"#, 13473);
}

#[test]
fn a_view_parameter_returns_an_owned_value() {
    // The callee returns a plain List(Int): [4,5] (2) versus root [4,5,6] (3).
    assert_wasm(r#"
fn snapshot(v: &List(Int)) -> List(Int):
    v.owned()

fn main() -> Int:
    var xs = [4, 5]
    let copy = snapshot(&xs)
    xs.push(6)
    copy.length() * 10 + xs.length()
"#, 23);
}

#[test]
fn an_owned_value_is_its_own_materialization() {
    // Identity: 3 elements, then 7: 3 * 10 + 7.
    assert_wasm(r#"
fn main() -> Int:
    let xs = [1, 2, 3]
    let same: List(Int) = xs.owned()
    let n = 7
    same.length() * 10 + n.owned()
"#, 37);
}

#[test]
fn a_generic_view_materializes_and_a_types_own_owned_method_wins() {
    // Box.owned is the type's own method: 41 + 1 = 42. The generic
    // materializer copies "hey" (3) before the root becomes "no" (2), and 42.
    assert_wasm(r#"
type Box:
    n: Int

impl Box:
    fn owned(self) -> Int:
        self.n + 1

fn copy_of[a](x: &a) -> a:
    x.owned()

fn main() -> Int:
    let b = Box(n: 41)
    let k = b.owned()
    var s = "hey"
    let t = copy_of(&s)
    s = "no"
    let m = copy_of(&k)
    k * 10000 + t.length() * 1000 + m * 10 + s.length()
"#, 423422);
}

#[test]
fn generic_owned_is_checked_at_each_concrete_receiver_type() {
    // The owned Int call returns 7. At `a = &Int`, `.owned()` would return
    // Int, which cannot satisfy the declared `&Int` result.
    assert_wasm(r#"
fn copy[a](x: a) -> a:
    x.owned()

fn main() -> Int:
    copy(7)
"#, 7);

    assert_refused(r#"
fn copy[a](x: a) -> a:
    x.owned()

fn main() -> Int:
    let n = 7
    let v = copy(&n)
    *v
"#, "expected `&Int`, found `Int`");
}

#[test]
fn generic_owned_refuses_a_concrete_nested_view() {
    assert_refused(r#"
fn copy[a](xs: List(a)) -> List(a):
    xs.owned()

fn main() -> Int:
    let n = 7
    let xs = [&n]
    let copied = copy(xs)
    copied.length()
"#, "contains an immutable view");
}

#[test]
fn a_bounding_traits_owned_method_wins_on_a_generic_receiver() {
    // Dup.owned doubles: 21 * 2.
    assert_wasm(r#"
trait Dup:
    fn owned(self) -> Int

impl Dup for Int:
    fn owned(self) -> Int:
        self * 2

fn twice[a](x: a) -> Int where a: Dup:
    x.owned()

fn main() -> Int:
    twice(21)
"#, 42);
}

#[test]
fn an_unknown_receiver_type_is_refused() {
    assert_refused(r#"
fn main() -> Int:
    let f = fn(v): v.owned()
    f(3)
"#, "`.owned()` needs the receiver's type to be known here; annotate the receiver");
}

#[test]
fn a_slice_view_is_refused() {
    assert_refused(r#"
fn main() -> Int:
    let xs = [1, 2, 3]
    let s = &xs[0..2]
    let copy = s.owned()
    copy.length()
"#, "`.owned()` cannot materialize the slice view `&[Int]`");
}

#[test]
fn authority_is_refused() {
    assert_refused(r#"
fn main(console: Console):
    let other = console.owned()
    other.print("x")
"#, "`.owned()` cannot copy authority `Console`");
}

#[test]
fn a_must_payload_is_refused() {
    assert_refused(r#"
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n

fn main() -> Int:
    let ticket = Ticket(3)
    let copy = ticket.owned()
    finish(copy)
"#, "`.owned()` cannot copy must-consume");
}

#[test]
fn a_view_bearing_aggregate_is_refused() {
    assert_refused(r#"
type Holder:
    item: &Int

fn main() -> Int:
    let n = 3
    let h = Holder(item: &n)
    let copy = h.owned()
    *copy.item
"#, "`.owned()` cannot materialize `Holder`: it contains an immutable view");
    assert_refused(r#"
fn main() -> Int:
    let n = 3
    let xs = [&n]
    let copy = xs.owned()
    copy.length()
"#, "`.owned()` cannot materialize `List(&Int)`: it contains an immutable view");
}
