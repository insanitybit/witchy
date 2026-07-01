//! Differential fuzzer: the interpreter (a Rust-memory-safe oracle) must agree with the
//! compiled-WASM backend on randomly-generated, well-typed programs. This is the coverage
//! engine for "is the IR/WASM memory safe" — a codegen heap bug (wrong offset, missing
//! `ensure()`, list/string mis-layout) that corrupts a value shows up as a backend DIVERGE.
//! The generator leans on the heap-relevant ops: `${int}` rendering (the `int_to_string`
//! OOB class), string concatenation, and list/dict construction, with int magnitudes spread
//! across the i64 range to push allocations toward page boundaries.
//!
//! `witchy parity <file>` is the oracle: it runs both backends and prints `DIVERGE` on a real
//! mismatch (incl. interp-ok / compiled-trap), `agree (both error)` when both reject (so we
//! need not avoid runtime traps), and a plain exit-1 for a compile error (a generator miss,
//! tolerated). A crash (terminated by signal) means the host itself died — also a bug.
//!
//! Two RFC-0037 upgrades live here. (§2) CROSS-LEVER DIFFERENTIAL: every program is run under a
//! SET of `WITCHY_OPT` configurations (baseline `none`, the production default, each opt-in
//! lever ALONE, and the union). Because the interpreter oracle ignores `WITCHY_OPT`, all configs
//! agreeing with it means all compiled outputs agree with each other — so any lever that changes
//! observable behavior is a DIVERGE, not a silent survivor. This is the net that would have
//! caught the rc-floor use-after-free that hid for ~2 days. (§1) GRAMMAR-COMPLETE GENERATOR: the
//! generator now emits USER FUNCTIONS with `let`/`own` params, closures, direct + mutual
//! recursion, tuple-of-owned-buffer returns, and — the class that hid the UAF — a local `var`
//! that ALIAS-INITS a borrowed param (or another local) and then SELF-REFERENTIALLY REASSIGNS it,
//! with the shared value RE-READ afterward as a use-after-free trip-wire. A grammar-coverage
//! meta-assertion fails if any statement kind was never generated.

use std::io::Write;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// Number of statement kinds the generator can emit (see `gen_program`'s match). The
/// grammar-coverage meta-assertion requires every one of these to appear across a run.
const NKINDS: u32 = 32;

/// Deterministic splitmix-ish PRNG — reproducible runs (no wall clock / OS randomness).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn alnum(r: &mut Rng) -> String {
    let len = r.below(6);
    (0..len)
        .map(|_| {
            let cs = b"abcXYZ0_ ";
            cs[r.below(cs.len() as u64) as usize] as char
        })
        .collect()
}

fn gen_int(r: &mut Rng, depth: u32) -> String {
    if depth == 0 {
        // Leaves: literals only, spread across the i64 range (renders near page edges).
        match r.below(4) {
            0 => format!("({})", r.below(200) as i64 - 100),
            1 => format!("{}", r.next() % 1_000_000),
            2 => format!("({})", r.next() as i64),
            _ => format!("({})", i64::from_le_bytes(r.next().to_le_bytes())),
        }
    } else if r.below(6) < 5 {
        // `/` and `%` included — a zero divisor traps identically on both backends.
        let op = ["+", "-", "*", "/", "%"][r.below(5) as usize];
        format!("({} {} {})", gen_int(r, depth - 1), op, gen_int(r, depth - 1))
    } else {
        // `list.at` exercises indexed reads (out-of-range just traps on BOTH backends =
        // agreement, so it's free coverage of the bounds path). depth-1 bounds recursion.
        format!("list.at({}, {})", gen_intlist(r, depth - 1), gen_int(r, depth - 1))
    }
}

fn gen_float(r: &mut Rng, depth: u32) -> String {
    if depth == 0 || r.below(3) == 0 {
        match r.below(4) {
            0 => format!("{}.{}", r.below(1000), r.below(1000)),
            1 => "0.0".to_string(),
            2 => format!("math.to_float({})", gen_int(r, 0)),
            _ => format!("(0.0 - {}.{})", r.below(100), r.below(100)),
        }
    } else {
        let op = ["+", "-", "*", "/"][r.below(4) as usize];
        format!("({} {} {})", gen_float(r, depth - 1), op, gen_float(r, depth - 1))
    }
}

fn gen_bool(r: &mut Rng, depth: u32) -> String {
    match r.below(if depth == 0 { 3 } else { 5 }) {
        0 => format!("({} {} {})", gen_int(r, 1), ["<", "<=", "==", "!=", ">", ">="][r.below(6) as usize], gen_int(r, 1)),
        1 => format!("({} == {})", gen_str(r, 1), gen_str(r, 1)),
        2 => format!("string.contains({}, {})", gen_str(r, 1), gen_str(r, 1)),
        3 => format!("({} && {})", gen_bool(r, depth - 1), gen_bool(r, depth - 1)),
        _ => format!("(!{})", gen_bool(r, depth - 1)),
    }
}

/// `Some(int)` — exercises constructor/option value layout + __render. (Bare `None` can't
/// infer its type in `__render`, so we stick to `Some`, which is the heap-relevant case.)
fn gen_option(r: &mut Rng) -> String {
    format!("Some({})", gen_int(r, 1))
}

/// `if bool: int else: int` — exercises the conditional-expression lowering.
fn gen_cond_int(r: &mut Rng, depth: u32) -> String {
    format!("(if {}: {} else: {})", gen_bool(r, 1), gen_int(r, depth), gen_int(r, depth))
}

fn gen_str(r: &mut Rng, depth: u32) -> String {
    if depth == 0 || r.below(3) == 0 {
        match r.below(3) {
            0 => format!("\"{}\"", alnum(r)),
            1 => format!("\"x${{{}}}y\"", gen_int(r, depth.min(2))), // `${int}` -> int_to_string
            _ => "\"\"".to_string(),
        }
    } else {
        match r.below(4) {
            0 | 1 => format!("({} + {})", gen_str(r, depth - 1), gen_str(r, depth - 1)),
            2 => format!("string.to_upper({})", gen_str(r, depth - 1)),
            // arbitrary indices: an out-of-range substring traps identically on both backends.
            _ => format!("string.substring({}, {}, {})", gen_str(r, depth - 1), gen_int(r, 0), gen_int(r, 0)),
        }
    }
}

/// List(String), nested List(List(Int)), and a tuple — the heaviest heap-layout coverage.
fn gen_strlist(r: &mut Rng, depth: u32) -> String {
    let n = 1 + r.below(4);
    let elems: Vec<String> = (0..n).map(|_| gen_str(r, depth.min(2))).collect();
    format!("[{}]", elems.join(", "))
}

fn gen_nested_intlist(r: &mut Rng) -> String {
    let n = 1 + r.below(3);
    let elems: Vec<String> = (0..n).map(|_| gen_intlist(r, 1)).collect();
    format!("[{}]", elems.join(", "))
}

fn gen_tuple(r: &mut Rng) -> String {
    format!("({}, {})", gen_int(r, 1), gen_str(r, 1))
}

/// A `Dict(String, Int)` built by a chain of insert/remove/set_at over a SMALL key space,
/// so inserts/removes/reinserts collide — the remove+reinsert+iterate pattern that has
/// previously corrupted the compiled dict. (`dict.new()` alone is ambiguous like `[]`, so
/// start from a typed insert.)
fn gen_dkey(r: &mut Rng) -> String {
    format!("\"k{}\"", r.below(4))
}

fn gen_dict(r: &mut Rng, ops: u32) -> String {
    let mut d = format!("dict.insert(dict.new(), {}, {})", gen_dkey(r), gen_int(r, 1));
    for _ in 0..ops {
        d = match r.below(4) {
            0 => format!("dict.set_at({}, {}, {})", d, gen_dkey(r), gen_int(r, 1)),
            1 => format!("dict.remove({}, {})", d, gen_dkey(r)),
            _ => format!("dict.insert({}, {}, {})", d, gen_dkey(r), gen_int(r, 1)),
        };
    }
    d
}

/// Heap-allocated records: `R(Int, String, List(Int))` and the nested `P(R, Int)` — struct
/// layout + (nested) field access, the same class as the int_to_string / dict-corruption bugs.
fn gen_record_r(r: &mut Rng) -> String {
    format!("R({}, {}, {})", gen_int(r, 1), gen_str(r, 1), gen_intlist(r, 1))
}

fn gen_record_p(r: &mut Rng) -> String {
    let inner = gen_record_r(r);
    format!("P({}, {})", inner, gen_int(r, 1))
}

fn gen_intlist(r: &mut Rng, depth: u32) -> String {
    // Always >= 1 element: an empty `[]` has an ambiguous element type, which the structural
    // renderer can't build (a known interpreter-only limit) — that's a generator miss, not a
    // codegen bug, so avoid it and keep the programs compiling.
    let n = 1 + r.below(4);
    let elems: Vec<String> = (0..n).map(|_| gen_int(r, depth.min(2))).collect();
    format!("[{}]", elems.join(", "))
}

/// The fixed "risky-shape" helper library (RFC-0037 §1) prepended to every program. These are
/// the grammar the old generator could never produce: user functions with `let`/`own` params,
/// a local `var` that alias-inits a borrowed param then self-referentially reassigns it (the
/// exact use-after-free class), an `own`-buffer accumulator, a tuple-of-two-owned-buffers return
/// (the RFC-0036 executor shape), direct + mutual recursion, and a closure applied through a
/// function-typed parameter. Callers vary the ARGUMENTS (via `gen_*`); the shapes are fixed and
/// known-well-typed so they always compile and exercise codegen on both backends.
const HELPER_LIB: &str = "\
// --- RFC-0037 §1 risky-shape helper library ---\n\
fn alias_str(s: String) -> String:\n\
\x20   var t = s\n\
\x20   t = t + \"!\"\n\
\x20   t\n\
fn alias_list(xs: List(Int)) -> List(Int):\n\
\x20   var ys = xs\n\
\x20   ys = list.concat(ys, ys)\n\
\x20   ys\n\
fn alias_field(rr: R) -> String:\n\
\x20   var b = rr.b\n\
\x20   b = b + \"z\"\n\
\x20   b\n\
fn grow(own xs: List(Int), n: Int) -> List(Int):\n\
\x20   var ys = xs\n\
\x20   var i = 0\n\
\x20   while i < n:\n\
\x20       ys = list.push(ys, i)\n\
\x20       i = i + 1\n\
\x20   ys\n\
fn swap2(own a: List(Int), own b: List(Int)) -> (List(Int), List(Int)):\n\
\x20   (b, a)\n\
fn accum(n: Int) -> Int:\n\
\x20   if n <= 0:\n\
\x20       0\n\
\x20   else:\n\
\x20       n + accum(n - 1)\n\
fn even_(n: Int) -> Bool:\n\
\x20   if n <= 0:\n\
\x20       true\n\
\x20   else:\n\
\x20       odd_(n - 1)\n\
fn odd_(n: Int) -> Bool:\n\
\x20   if n <= 0:\n\
\x20       false\n\
\x20   else:\n\
\x20       even_(n - 1)\n\
fn apply_twice(f: fn(Int) -> Int, x: Int) -> Int:\n\
\x20   f(f(x))\n";

/// One random program: a `main` that prints many heap-exercising expressions, preceded by the
/// risky-shape helper library. Returns the source and a bitmask of which statement kinds it
/// emitted (bit `k` set iff kind `k` was chosen) for the grammar-coverage meta-assertion.
fn gen_program(seed: u64, statements: usize) -> (String, u64) {
    let mut r = Rng(seed);
    // Fixed record types so statements can construct + field-access heap structs (incl. a
    // nested record, the deepest heap-layout shape) without per-program type generation.
    let mut body = String::from(
        "import string\nimport list\nimport math\nimport dict\n\n\
         type R:\n    a: Int\n    b: String\n    c: List(Int)\n\
         type P:\n    x: R\n    y: Int\n\
         type Q:\n    m: Int\n    n: Int\n\n",
    );
    body.push_str(HELPER_LIB);
    body.push('\n');
    body.push_str("fn main(console: Console):\n");
    let mut used: u64 = 0;
    for stmt_i in 0..statements {
        let kind = r.below(NKINDS as u64);
        used |= 1u64 << kind;
        let depth = 1 + r.below(4) as u32;
        let dops = 2 + r.below(10) as u32;
        let line = match kind {
            0 => format!("    print(console, __render({}))\n", gen_int(&mut r, depth)),
            1 => format!("    print(console, {})\n", gen_str(&mut r, depth)),
            2 => format!("    print(console, __render({}))\n", gen_intlist(&mut r, 2)),
            3 => format!("    print(console, \"${{string.length({})}}\")\n", gen_str(&mut r, 2)),
            4 => format!("    print(console, \"${{list.length({})}}\")\n", gen_intlist(&mut r, 2)),
            5 => format!("    print(console, __render({}))\n", gen_float(&mut r, depth)),
            6 => format!("    print(console, __render({}))\n", gen_bool(&mut r, depth)),
            7 => format!("    print(console, __render({}))\n", gen_option(&mut r)),
            8 => format!("    print(console, __render({}))\n", gen_cond_int(&mut r, depth)),
            9 => format!("    print(console, __render({}))\n", gen_strlist(&mut r, depth)),
            10 => format!("    print(console, __render({}))\n", gen_nested_intlist(&mut r)),
            11 => format!("    print(console, __render({}))\n", gen_tuple(&mut r)),
            12 => format!("    print(console, __render({}))\n", gen_dict(&mut r, dops)),
            13 => format!("    print(console, \"${{dict.length({})}}\")\n", gen_dict(&mut r, dops)),
            14 => format!("    print(console, __render(dict.pairs({})))\n", gen_dict(&mut r, dops)),
            15 => {
                let d = gen_dict(&mut r, dops);
                let k = gen_dkey(&mut r);
                format!("    print(console, __render(dict.get_or({}, {}, (-1))))\n", d, k)
            }
            16 => format!("    print(console, __render({}))\n", gen_record_r(&mut r)),
            17 => format!("    print(console, __render({}.a))\n", gen_record_r(&mut r)),
            18 => format!("    print(console, {}.b)\n", gen_record_r(&mut r)),
            19 => format!("    print(console, __render({}.c))\n", gen_record_r(&mut r)),
            20 => format!("    print(console, __render({}))\n", gen_record_p(&mut r)),
            21 => {
                // A confined `let`-bound list of a PACKABLE record (`Q { m, n }`),
                // read only via `at(_).field` / `length` — the shape the packed
                // `unbox` codegen flattens. With `WITCHY_OPT=all` (set on the run)
                // this exercises the flat-buffer heap-layout path under the checked
                // heap. Index stays in-bounds (`below(m)`) so both backends agree.
                let m = 2 + r.below(3);
                let elems: Vec<String> = (0..m)
                    .map(|_| format!("Q({}, {})", gen_int(&mut r, 1), gen_int(&mut r, 1)))
                    .collect();
                let j = r.below(m);
                format!(
                    "    let qk{stmt_i} = [{}]\n    print(console, __render(list.at(qk{stmt_i}, {}).m + list.at(qk{stmt_i}, {}).n + list.length(qk{stmt_i})))\n",
                    elems.join(", "),
                    j,
                    j,
                )
            }
            22 => {
                // A confined `var` reassigned to SAME-LENGTH list literals in a loop,
                // read only via at/length — the shape the RC-floor `rc-elide` rung
                // overwrites in place. With all opts on (set on the run) this fuzzes
                // the in-place-overwrite Store offset math under the checked heap.
                // Elements are fresh int exprs (never read the var → no self-ref bail).
                let l = 2 + r.below(3);
                // Initial length differs from the loop length (sometimes), so the
                // capacity-resizing path — realloc on first iteration, then reuse —
                // is exercised alongside the same-length in-place overwrite.
                let init = 1 + r.below(l);
                let zeros = vec!["0"; init as usize].join(", ");
                let iters = 3 + r.below(5);
                let elems: Vec<String> = (0..l).map(|_| gen_int(&mut r, 1)).collect();
                format!(
                    "    var rv{stmt_i} = [{}]\n    var rj{stmt_i} = 0\n    while rj{stmt_i} < {}:\n        rv{stmt_i} = [{}]\n        rj{stmt_i} = rj{stmt_i} + 1\n    print(console, __render(list.at(rv{stmt_i}, 0) + list.length(rv{stmt_i})))\n",
                    zeros,
                    iters,
                    elems.join(", "),
                )
            }
            23 => format!("    print(console, __render({}.x.a))\n", gen_record_p(&mut r)),
            24 => {
                // Borrow a String into a helper that alias-inits + self-ref-reassigns it, then
                // RE-READ the shared arg afterward — the exact use-after-free trip-wire. Under a
                // bad free-at-overwrite the re-read of `sv` sees freed bytes and DIVERGES.
                let s = gen_str(&mut r, depth.min(2));
                format!(
                    "    let sv{stmt_i} = {s}\n    let av{stmt_i} = alias_str(sv{stmt_i})\n    print(console, sv{stmt_i})\n    print(console, av{stmt_i})\n"
                )
            }
            25 => {
                // Same trip-wire over a heap List: alias-init + self-ref concat inside the
                // helper, then re-read the borrowed list.
                let l = gen_intlist(&mut r, 2);
                format!(
                    "    let lv{stmt_i} = {l}\n    print(console, __render(alias_list(lv{stmt_i})))\n    print(console, __render(lv{stmt_i}))\n"
                )
            }
            26 => {
                // Alias a heap FIELD (`var b = rr.b`) then reassign the local; re-read the field.
                let rec = gen_record_r(&mut r);
                format!(
                    "    let fr{stmt_i} = {rec}\n    print(console, alias_field(fr{stmt_i}))\n    print(console, fr{stmt_i}.b)\n"
                )
            }
            27 => {
                // Statement-level confined alias-init + self-ref reassignment, re-reading the
                // original — the same class without a function boundary.
                let s1 = gen_str(&mut r, 1);
                let s2 = gen_str(&mut r, 1);
                format!(
                    "    var sc{stmt_i} = {s1}\n    sc{stmt_i} = sc{stmt_i} + {s2}\n    var tc{stmt_i} = sc{stmt_i}\n    tc{stmt_i} = string.to_upper(tc{stmt_i})\n    print(console, sc{stmt_i})\n    print(console, tc{stmt_i})\n"
                )
            }
            28 => {
                // `own`-buffer accumulator threaded through a loop (consumes a fresh literal).
                let l = gen_intlist(&mut r, 1);
                let n = r.below(5);
                format!("    print(console, __render(grow({l}, {n})))\n")
            }
            29 => {
                // Tuple of two OWNED buffers returned + destructured (the RFC-0036 executor shape).
                let a = gen_intlist(&mut r, 1);
                let b = gen_intlist(&mut r, 1);
                format!(
                    "    let (sp{stmt_i}, sq{stmt_i}) = swap2({a}, {b})\n    print(console, __render(sp{stmt_i}))\n    print(console, __render(sq{stmt_i}))\n"
                )
            }
            30 => {
                // Direct + mutual recursion (small bounded depth).
                let n1 = r.below(18);
                let n2 = r.below(18);
                format!("    print(console, __render(accum({n1})))\n    print(console, __render(even_({n2})))\n")
            }
            _ => {
                // Closure captured + applied through a function-typed parameter.
                let c = r.below(20) as i64 - 10;
                let x = r.below(20) as i64 - 10;
                format!(
                    "    let cl{stmt_i} = fn(z: Int) -> Int: z + ({c})\n    print(console, __render(apply_twice(cl{stmt_i}, ({x}))))\n"
                )
            }
        };
        body.push_str(&line);
    }
    (body, used)
}

/// The cross-lever config set (RFC-0037 §2). Every program is run under each: the baseline
/// (`none`, all opts off), the production default (``), each opt-in lever ALONE (`rc-floor`,
/// `unbox` — the class that hid the UAF is exercised in isolation, not just masked inside the
/// union), and the union (`all,-wasm-opt`; `-wasm-opt` omits the external Binaryen post-pass).
const CONFIGS: &[&str] = &["none", "", "rc-floor", "unbox", "all,-wasm-opt"];

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
fn differential_fuzz_interpreter_vs_compiled() {
    // Counts are env-overridable so the scheduled/`--full` job can scale coverage up. The
    // defaults keep total work (programs × statements × CONFIGS) close to the pre-cross-lever
    // budget while adding per-lever coverage.
    let programs = env_usize("WITCHY_FUZZ_PROGRAMS", 30);
    let statements = env_usize("WITCHY_FUZZ_STATEMENTS", 100);
    let mut agree = 0usize;
    let mut compile_skipped = 0usize;
    let mut kinds_used: u64 = 0;
    for seed in 0..programs as u64 {
        let (src, used) = gen_program(seed.wrapping_mul(0x1234_5678_9ABC_DEF1).wrapping_add(1), statements);
        kinds_used |= used;
        let path = std::env::temp_dir().join(format!("witchy_fuzz_{seed}.witchy"));
        std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
        // Cross-lever differential: `parity` reads `WITCHY_OPT` for its compiled side; the
        // interpreter oracle ignores it. So running parity under each config compares that
        // config's compiled output against the one constant oracle — all-agree ⇒ all compiled
        // outputs agree with each other. Any lever that changes observable behavior is a DIVERGE.
        for &cfg in CONFIGS {
            let out = Command::new(BIN)
                .args(["parity", path.to_str().unwrap()])
                .env("WITCHY_OPT", cfg)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let label = if cfg.is_empty() { "<default>" } else { cfg };

            // A crash (terminated by signal -> no exit code) means the host process itself died.
            assert!(
                out.status.code().is_some(),
                "witchy crashed (signal) on seed {seed} under WITCHY_OPT={label} — host-level memory unsafety.\n--- program ---\n{src}\n--- stderr ---\n{stderr}"
            );
            if stdout.contains("DIVERGE") || stderr.contains("DIVERGE") {
                panic!(
                    "BACKENDS DIVERGE on seed {seed} under WITCHY_OPT={label} — a codegen/memory bug (an optimization changed observable behavior).\n--- program ---\n{src}\n--- output ---\n{stdout}{stderr}"
                );
            }
            // Account compile/agree once, on the production-default config, to gauge the generator.
            if cfg.is_empty() {
                if out.status.success() {
                    agree += 1;
                } else {
                    // Non-DIVERGE exit-1 = the generator produced a non-compiling program; tolerated.
                    compile_skipped += 1;
                }
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    // Sanity: the generator must mostly produce compiling programs, or it isn't testing codegen.
    assert!(
        agree * 2 >= programs,
        "fuzzer mostly produced non-compiling programs ({agree} agree, {compile_skipped} skipped) — fix the generator"
    );
    // Grammar-coverage meta-assertion (RFC-0037 §1): every statement kind the generator CAN emit
    // must actually have appeared across the run — turning "did we cover the grammar" from an
    // assumption into a test (this is what would have flagged "we never generate user functions").
    // Only enforced when enough statements were generated to make full coverage near-certain.
    if programs * statements >= 1500 {
        let expected = (1u64 << NKINDS) - 1;
        let missing = expected & !kinds_used;
        assert_eq!(
            missing, 0,
            "generator never emitted statement kind(s) (missing bitmask {missing:#034b}) — a grammar-coverage hole; raise counts or fix the generator"
        );
    }
    eprintln!(
        "differential fuzz: {agree} agree, {compile_skipped} compile-skipped of {programs} programs × {} configs; kinds covered {kinds_used:#034b}",
        CONFIGS.len()
    );
}
