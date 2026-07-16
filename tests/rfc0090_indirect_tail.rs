//! RFC-0090 Stage 3 — GENUINELY-INDIRECT proper tail calls.
//!
//! A recursive cycle that passes through a function VALUE (an indirect
//! `call_indirect` through the closure table) must execute with constant control
//! stack on BOTH backends, without erasing ExternRef / GcRef / scalar kinds,
//! closure environments, or callable result kinds. The direct/mutual named cases
//! are covered by `rfc0090_reference_tail*.rs`; this file is the indirect analog.
//!
//! Every cycle here routes through a `fn(...) -> ...` PARAMETER: `driver` receives
//! a function value `k` and tail-calls it; the passed function (`bump`) tail-calls
//! a `make` that tail-calls `driver` again — so the recursive edge crosses a
//! genuinely indirect call whose target is only known through the table. The
//! wir_opt SCC dispatcher adds every exact-signature table target to the recursive
//! component and lowers the whole cycle to one portable trampoline loop
//! (`wasm_tail_call(false)` stays configured; no `return_call` is emitted).
//!
//! These are RESOURCE + PARITY tests: the transition count (250,000) is ~30x
//! the MEASURED depth at which a non-tail cycle exhausts the compiled backend's
//! call stack (traps between 6,000 and 8,000 frames; probed 2026-07-16 with the
//! same cycle shape made non-tail via `k(n) + 0`), so a green compiled run is
//! itself the constant-stack proof with a wide margin. The interpreter leg
//! proves RESULT PARITY at the same depth; it cannot fail by overflow at any
//! practical count (its call frames are heap-allocated — measured fine at 500k
//! non-tail frames), so interp constant-stack behavior is proven by the
//! dedicated witchy-interp unit tests (deep_self_tail_recursion_*), not here.
//! 5,000,000 was the original count; it bought no additional proof strength and
//! cost ~30-50s of CPU per test per gate. Kind
//! preservation is proven by the programs being well-typed and WASM-validating with
//! ExternRef / GC-reference parameters and results that never pass through an i64
//! slot (a boxed reference would fail validation or diverge).

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

/// Run `source` on BOTH backends and assert identical output. `dir_root`/`dir_read`
/// grant a read-only capability root when the program takes a `Dir[Read]`. A
/// program that grows the control stack instead of tail-looping overflows here
/// (the compiled backend traps "call stack exhausted"; the interpreter hits its
/// depth guard) — so agreement at the high transition counts below is the
/// constant-stack guarantee, not merely value equality.
fn assert_both_backends(source: &str, expected: &[&str], root: Option<&std::path::Path>) {
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("indirect tail program typechecks");

    let want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    let root_string = root
        .map(|r| r.to_str().expect("UTF-8 root").to_string())
        .unwrap_or_else(|| ".".to_string());

    assert_eq!(
        interpreter::run_module(linked.clone(), &root_string, Vec::new()).expect("interpret"),
        want,
        "interpreter callable-boundary loop (indirect cycle must be constant-stack)",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("indirect tail program lowers to WIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities {
        print: true,
        quiet: true,
        dir_root: root.map(|r| r.to_path_buf()),
        dir_read: root.is_some(),
        ..Default::default()
    };
    // Cap linear memory at the CLI's `RUN_MEMORY_PAGES` (16384 = 1 GiB max, grown
    // on demand). A proper tail cycle is constant-STACK, but 250,000 transitions
    // still churn per-iteration heap for the reference/tuple cases, so the small
    // 64-page cap the 300k-iteration reference test uses is not enough headroom.
    let mut actor = runtime.spawn(&wasm, caps, 16384).expect("spawn");
    actor.run().expect("compiled execution");
    assert_eq!(
        actor.output(),
        want,
        "compiled typed indirect dispatcher (constant-stack trampoline)"
    );
}

/// A temp `Dir[Read]` root seeded with `value.txt`, for the reference-carrying cases.
fn seed_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("witchy_indirect_tail_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create capability root");
    std::fs::write(root.join("value.txt"), "indirect-tail").expect("seed file");
    root
}

// (1) A scalar closure-table cycle performs at least 250,000 transitions on both
// backends. `driver` tail-calls the function value `k`; `bump -> make -> driver`
// closes the cycle through the table, so the recursive edge is genuinely indirect.
#[test]
fn scalar_indirect_cycle_is_constant_stack_at_scale() {
    let source = r#"
fn driver(k: fn(Int) -> Int, n: Int) -> Int:
    if n <= 0:
        0
    else:
        k(n)

fn make(n: Int) -> Int:
    driver(bump, n - 1)

fn bump(n: Int) -> Int:
    make(n)

fn main(console: Console):
    console.print("${driver(bump, 250000)}")
"#;
    assert_both_backends(source, &["0"], None);
}

// (2) An indirect cycle carries an ExternRef parameter (a `Dir[Read]` capability)
// without integer-slot boxing. The capability crosses the indirect edge 250k times;
// a boxed externref would fail WASM validation or diverge.
#[test]
fn externref_parameter_survives_indirect_cycle() {
    let root = seed_root("externref");
    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn driver(k: fn(Dir[Read], Int) -> String, dir: Dir[Read], n: Int) -> String:
    if n <= 0:
        read_named(dir, "value.txt")
    else:
        k(dir, n)

fn make(dir: Dir[Read], n: Int) -> String:
    driver(bump, dir, n - 1)

fn bump(dir: Dir[Read], n: Int) -> String:
    make(dir, n)

fn main(console: Console, root: Dir[Read]):
    console.print(driver(bump, root, 250000))
"#;
    assert_both_backends(source, &["indirect-tail"], Some(&root));
    let _ = std::fs::remove_dir_all(&root);
}

// (3) An indirect cycle carries a concrete GC-reference tuple (a
// `(Dir[Read], String)` aggregate) through the dispatcher, 250k transitions.
#[test]
fn gc_reference_tuple_parameter_survives_indirect_cycle() {
    let root = seed_root("gctuple");
    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn driver(k: fn((Dir[Read], String), Int) -> String, pair: (Dir[Read], String), n: Int) -> String:
    if n <= 0:
        read_named(pair.0, pair.1)
    else:
        k(pair, n)

fn make(pair: (Dir[Read], String), n: Int) -> String:
    driver(bump, pair, n - 1)

fn bump(pair: (Dir[Read], String), n: Int) -> String:
    make(pair, n)

fn main(console: Console, root: Dir[Read]):
    console.print(driver(bump, (root, "value.txt"), 250000))
"#;
    assert_both_backends(source, &["indirect-tail"], Some(&root));
    let _ = std::fs::remove_dir_all(&root);
}

// (4) A reference-valued RESULT survives the indirect dispatcher unchanged: the
// cycle threads a `(Dir[Read], String)` aggregate as both parameter AND result,
// returning it after 250k indirect transitions, then projects its String field.
#[test]
fn reference_valued_result_survives_indirect_dispatcher() {
    let root = seed_root("refresult");
    let source = r#"
fn driver(k: fn((Dir[Read], String), Int) -> (Dir[Read], String), pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    if n <= 0:
        pair
    else:
        k(pair, n)

fn make(pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    driver(bump, pair, n - 1)

fn bump(pair: (Dir[Read], String), n: Int) -> (Dir[Read], String):
    make(pair, n)

fn label(pair: (Dir[Read], String)) -> String:
    match pair:
        (_dir, name) -> name

fn main(console: Console, root: Dir[Read]):
    let out = driver(bump, (root, "kept"), 250000)
    console.print(label(out))
"#;
    assert_both_backends(source, &["kept"], Some(&root));
    let _ = std::fs::remove_dir_all(&root);
}

// (5) A mixed scalar/reference signature retains its exact parameter and result
// kinds across the indirect edge: `(Dir[Read], Int, String)` in, `String` out.
#[test]
fn mixed_scalar_reference_signature_keeps_exact_kinds() {
    let root = seed_root("mixed");
    let source = r#"
fn read_named(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn driver(k: fn(Dir[Read], Int, String) -> String, dir: Dir[Read], n: Int, name: String) -> String:
    if n <= 0:
        read_named(dir, name)
    else:
        k(dir, n, name)

fn make(dir: Dir[Read], n: Int, name: String) -> String:
    driver(bump, dir, n - 1, name)

fn bump(dir: Dir[Read], n: Int, name: String) -> String:
    make(dir, n, name)

fn main(console: Console, root: Dir[Read]):
    console.print(driver(bump, root, 250000, "value.txt"))
"#;
    assert_both_backends(source, &["indirect-tail"], Some(&root));
    let _ = std::fs::remove_dir_all(&root);
}

// (7) Argument evaluation is left-to-right and parameter rebinding is SIMULTANEOUS.
// The indirect edge swaps its two scalar arguments each transition (`k(b, a, n)`);
// a sequential (non-simultaneous) rebind would clobber `a` before `b` reads it,
// changing the parity of the swap. With an even transition count the arguments
// return to their original positions, so `a - b` at the base is deterministic.
#[test]
fn simultaneous_rebind_across_indirect_swap() {
    let source = r#"
fn driver(k: fn(Int, Int, Int) -> Int, a: Int, b: Int, n: Int) -> Int:
    if n <= 0:
        a - b
    else:
        k(b, a, n)

fn make(a: Int, b: Int, n: Int) -> Int:
    driver(bump, a, b, n - 1)

fn bump(a: Int, b: Int, n: Int) -> Int:
    make(a, b, n)

fn main(console: Console):
    console.print("${driver(bump, 7, 3, 250000)}")
"#;
    // 250,000 is even (as was the original 5,000,000), so the (a, b) swap count is
    // even and a=7,b=3 at the base:
    // 7 - 3 = 4. A non-simultaneous rebind would corrupt one side and diverge.
    assert_both_backends(source, &["4"], None);
}

// (8) A table target OUTSIDE the recursive component remains an ordinary indirect
// exit. `finish` is a function value that is NOT part of the cycle; it is called
// once at the base case through the table and must produce the correct value
// without being pulled into the trampoline component.
#[test]
fn out_of_component_target_is_ordinary_indirect_exit() {
    let source = r#"
fn driver(k: fn(Int) -> Int, done: fn(Int) -> Int, n: Int) -> Int:
    if n <= 0:
        done(n)
    else:
        k(n)

fn make(n: Int) -> Int:
    driver(bump, finish, n - 1)

fn bump(n: Int) -> Int:
    make(n)

fn finish(n: Int) -> Int:
    n + 42

fn main(console: Console):
    console.print("${driver(bump, finish, 250000)}")
"#;
    // Cycle runs to n <= 0 through `bump`, then the out-of-component `finish(0)`
    // exits with 0 + 42 = 42.
    assert_both_backends(source, &["42"], None);
}

// (9) A near-tail indirect call with residual computation (`1 + k(n)`) is NOT
// rewritten to a constant-stack loop — the addition is real work after the call,
// so the edge is not proper. At a BOUNDED depth it must still compute the correct
// value on both backends (this is a correctness, not a resource, test — the point
// is that the classifier declines the edge, not that it runs deep).
#[test]
fn near_tail_with_residual_is_not_rewritten() {
    let source = r#"
fn driver(k: fn(Int) -> Int, n: Int) -> Int:
    if n <= 0:
        0
    else:
        1 + k(n)

fn make(n: Int) -> Int:
    driver(bump, n - 1)

fn bump(n: Int) -> Int:
    make(n)

fn main(console: Console):
    console.print("${driver(bump, 100)}")
"#;
    // Each level adds 1 while n counts 100 -> 0 through `make`'s `n - 1`, so the
    // accumulated residual is 100. If this were wrongly tail-looped the residual
    // additions would be dropped and the result would be 0.
    assert_both_backends(source, &["100"], None);
}
