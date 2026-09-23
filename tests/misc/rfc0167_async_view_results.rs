//! RFC-0167 section 5 / rows G and K: an `async fn` may return a view, and a
//! closure (or an awaited task's recipe) may capture a `&[T]` slice view.
//!
//! A view is a value-semantic snapshot handle that retains its own root, so
//! the task result carries the view's carrier and the awaited value reads the
//! snapshot taken before the callee suspended, whatever the caller does to the
//! root meanwhile. A captured `&[T]` is its rooted-slice owner, retained into
//! an owner field of the closure environment.
//!
//! Evidence: an independent hand-computed expectation and admitted
//! unoptimized Wasm with a zero-leak heap audit.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_wasm_output(label: &str, source: &str, expected: &[&str]) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} type-checks: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered(&format!("{label} admits through canonical SafeWIR"));

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    vm.run().unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(vm.output(), expected, "{label} admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "{label} admitted Wasm releases all roots");
}

/// Each async test is one program and one async compile, kept small so it
/// stays within the gate's per-test time budget.
///
/// Each `keep_*` suspends before returning its view, and the caller mutated
/// the root after forming the view, so the awaited value must be the
/// snapshot: `&Int` 21 (root now 5); `&[Int]` over `[10, 20, 30, 40][1..3]`
/// is `[20, 30]` although the root's index 1 became 100.
#[test]
fn async_fns_return_owner_carried_views() {
    assert_wasm_output(
        "async owner-carried view results",
        r#"
import chan

async fn keep_int(v: &Int) -> &Int:
    chan.yield_now().await
    v

async fn keep_slice(v: &[Int]) -> &[Int]:
    chan.yield_now().await
    v

async fn main(console: Console):
    var n = 21
    let vn = &n
    n = 5
    let a = keep_int(vn).await
    console.print("${*a} ${n}")
    var ys = [10, 20, 30, 40]
    let sl = &ys[1..3]
    ys[1] = 100
    let e = keep_slice(sl).await
    var sum = 0
    for x in e:
        sum = sum + x
    console.print("${e.length()} ${sum} ${ys[1]}")
"#,
        // The slice snapshot is [20, 30] (sum 50) although ys[1] became 100.
        &["21 5", "2 50 100"],
    );
}

/// A task's recipe captures its `&[T]` parameter. An awaited function sums
/// `[10, 20, 30, 40][0..2]` = 30 after suspending, although the caller wrote
/// 100 at index 0 before awaiting. A spawned task holds `&ys[2..4]` =
/// `[30, 40]` across its yield while the caller writes 7 at index 2, so it
/// prints `2 30` after `spawned 7`.
#[test]
fn tasks_carry_slice_views_across_suspension() {
    assert_wasm_output(
        "tasks carry slice views",
        r#"
import chan

async fn sum_slice(v: &[Int]) -> Int:
    chan.yield_now().await
    var t = 0
    for x in v:
        t = t + x
    t

async fn worker(console: Console, xs: &[Int]):
    chan.yield_now().await
    var sum = 0
    for x in xs:
        sum = sum + x
    console.print("${xs.length()} ${sum}")

async fn main(console: Console):
    var ys = [10, 20, 30, 40]
    let pending = sum_slice(&ys[0..2])
    ys[0] = 100
    let total = pending.await
    console.print("${total}")
    let h = chan.spawn(worker(console, &ys[2..4])).await
    ys[2] = 7
    console.print("spawned ${ys[2]}")
    chan.join(h).await
"#,
        // The worker's slice snapshot is [30, 40] (sum 70) although ys[2] became 7.
        &["30", "spawned 7", "2 70"],
    );
}

/// Token-carried whole-root views returned after a suspension: `&String`
/// "abc" has length 3 (root now "zz"); `&List` has length 3 (root grew to
/// 4) although its task was held unawaited across a suspension; the record
/// view reads 3 + 4 = 7 (root now has x 9). A local named `keep_point`
/// shadows the declaration, so its awaited value is the `&String` "abc".
#[test]
fn async_fns_return_token_carried_views() {
    assert_wasm_output(
        "async token-carried view results",
        r#"
import chan

type Point:
    x: Int
    y: Int

async fn keep_str(v: &String) -> &String:
    chan.yield_now().await
    v

async fn keep_list(v: &List(Int)) -> &List(Int):
    chan.yield_now().await
    v

async fn keep_point(v: &Point) -> &Point:
    chan.yield_now().await
    v

async fn main(console: Console):
    var s = "abc"
    let vs = &s
    s = "zz"
    let b = keep_str(vs).await
    console.print("${(*b).length()} ${s}")
    var xs = [1, 2, 3]
    let vx = &xs
    let pending = keep_list(vx)
    xs.push(4)
    chan.yield_now().await
    let c = pending.await
    console.print("${(*c).length()} ${xs.length()}")
    var p = Point(x: 3, y: 4)
    let vp = &p
    p = Point(x: 9, y: 9)
    let d = keep_point(vp).await
    console.print("${(*d).x + (*d).y} ${p.x}")
    let keep_point = fn() -> task.Task(&String): keep_str(vs)
    let w = keep_point().await
    console.print(*w)
"#,
        &["3 zz", "3 4", "7 9", "abc"],
    );
}

/// A closure captures a `&[T]` of every admitted element and keeps its
/// snapshot while the `var` root is mutated after capture:
/// `[1, 2, 3, 4][1..3]` = `[2, 3]` gives 2 * 10 + 2 = 22 although index 1
/// became 100; a slice captured inside a helper and returned (`[3, 4]`) gives
/// 32 although index 2 became 50; Float `1.5 + 2.5` = 4.0; Bool `[false,
/// true][0]` = false; Duration length 1 after a push; String `"ab" + "cde"`.
/// A callable whose signature carries views (`fn(&String) -> &String`) is
/// called indirectly: `"abc" + "de"` has length 5.
#[test]
fn closures_capture_slice_views_of_every_admitted_element() {
    assert_wasm_output(
        "slice captures",
        r#"
fn make(s: &[Int]) -> fn() -> Int:
    fn() -> Int: s[0] * 10 + s.length()

fn relay(view: &String) -> &String:
    view

fn apply(f: fn(&String) -> &String, v: &String) -> Int:
    (*f(v)).length()

fn main(console: Console):
    var xs = [1, 2, 3, 4]
    let s = &xs[1..3]
    let f = fn() -> Int: s[0] * 10 + s.length()
    xs.push(9)
    xs[1] = 100
    console.print("${f()} ${xs[1]}")
    let g = make(&xs[2..4])
    xs[2] = 50
    console.print("${g()}")
    var fs = [1.5, 2.5, 3.5]
    let fv = &fs[0..2]
    let ff = fn() -> Float: fv[0] + fv[1]
    fs[0] = 100.0
    console.print("${ff()}")
    var bs = [true, false, true]
    let bv = &bs[1..3]
    let bf = fn() -> Bool: bv[0]
    bs[1] = true
    console.print("${bf()}")
    var ds = [1s, 2s, 3s]
    let dv = &ds[1..2]
    let df = fn() -> Int: dv.length()
    ds.push(4s)
    console.print("${df()}")
    var ws = ["ab", "cde", "f"]
    let wv = &ws[0..2]
    let wf = fn() -> String: wv[0] + wv[1]
    ws[0] = "zzzz"
    console.print(wf())
    let text = "abc" + "de"
    console.print("${apply(relay, &text)}")
"#,
        &["22 100", "32", "4.0", "false", "1", "abcde", "5"],
    );
}

/// A region-born root still cannot escape through a task result or a
/// captured slice (RFC-0167 section 8; BUG-646).
#[test]
fn region_born_views_cannot_escape_through_task_results_or_slice_captures() {
    for (label, source) in [
        ("task result", r#"
from task import Task

async fn keep(v: &Int) -> &Int:
    v

async fn main(console: Console):
    let t = region -> Task(&Int):
        let local = 2 + 3
        keep(&local)
    let r = t.await
    console.print("${*r}")
"#),
        ("optional task result", r#"
from task import Task

async fn keep(v: &[Int]) -> &[Int]:
    v

async fn main(console: Console):
    let t = region -> Option(Task(&[Int])):
        let local = [1, 2, 3]
        Some(keep(&local[0..2]))
    match t:
        Some(task) ->
            let r = task.await
            console.print("${r.length()}")
        None -> console.print("none")
"#),
        ("slice capture", r#"
fn keep(s: &[Int]) -> fn() -> Int:
    fn() -> Int: s[0]

fn main(console: Console):
    let f = region -> fn() -> Int:
        let local = [1, 2, 3]
        keep(&local[0..2])
    console.print("${f()}")
"#),
    ] {
        let error = witchy::resolve_std_only_checked(source).expect_err(label);
        assert!(error.to_string().contains("region-born root"), "{label}: {error}");
    }
}

/// Slice-carrier recognition keys on the checker's private `Slice` payload
/// name, so a source type may not take that name.
#[test]
fn a_source_type_cannot_take_the_reserved_slice_name() {
    let error = witchy::resolve_std_only_checked(
        "type Slice(a):\n    items: List(a)\n\nfn main() -> Int:\n    let s = Slice(items: [1, 2])\n    s.items.length()\n",
    )
    .err()
    .map(|error| error.to_string())
    .expect("a type named Slice is refused");
    assert!(error.contains("the name `Slice` is reserved"), "diagnostic names the rule: {error}");
}
