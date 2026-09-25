//! RFC-0167 §5 row G: `.owned()` explicitly materializes an independent owned
//! value. It is compiler-known (no import): on a view `&T` it reads the
//! referent exactly as `*view` does, and on an owned copyable value it is
//! identity. The result carries no view, so later mutation of either the root
//! or the copy is invisible to the other. Slices with a proved element
//! carrier materialize a fresh list; authority, `must` payloads, and
//! view-bearing aggregates are refused while checking.
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
fn a_generic_list_view_materializes_only_after_a_concrete_storage_proof() {
    // The copy has [4,5,7], the mutated owner has [4,5,6], and the view
    // retains the original two-element root: 4000 + 300 + 30 + 2.
    assert_wasm(r#"
fn copy[a](items: &List(a)) -> List(a):
    items.owned()

fn main() -> Int:
    var items = [4, 5]
    let view = &items
    var copy = copy(view)
    items.push(6)
    copy.push(7)
    copy[0] * 1000 + copy.length() * 100 + items.length() * 10 + (*view).length()
"#, 4332);

    assert_refused(r#"
fn copy[a](items: List(a)) -> List(a):
    items.owned()

fn main(console: Console):
    let copied = copy([console])
    copied[0].print("x")
"#, "`.owned()` cannot copy authority");
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
