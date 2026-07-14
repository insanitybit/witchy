//! Differential fuzzer: the interpreter (a Rust-memory-safe oracle) must agree with the
//! compiled-WASM backend on randomly-generated, well-typed programs. This is the coverage
//! engine for "is the IR/WASM memory safe" — a codegen heap bug (wrong offset, missing
//! `ensure()`, list/string mis-layout) that corrupts a value shows up as a backend DIVERGE.
//! The generator leans on the heap-relevant ops: `${int}` rendering (the `int_to_string`
//! OOB class), string concatenation, and list/dict construction, with int magnitudes spread
//! across the i64 range to push allocations toward page boundaries.
//!
//! `witchy parity <file>` is the oracle: it runs both backends and prints `DIVERGE` on a real
//! mismatch (incl. interp-ok / compiled-trap), `agree (both error)` when both
//! reject with the same diagnostic (so we need not avoid runtime traps), and an
//! `unexpected-error` exit for a compile error. A compile miss is
//! tolerated only when `witchy check` rejects the same generated source; if `check` accepts it,
//! every optimizer config must reach both backends. A crash (terminated by signal) means the
//! host itself died — also a bug.
//!
//! Two RFC-0037 upgrades live here. (§2) CROSS-LEVER DIFFERENTIAL: every program is run under a
//! SET of `WITCHY_OPT` configurations (baseline `none`, the production default, each opt-in
//! lever ALONE, and the union). Because the interpreter oracle ignores `WITCHY_OPT`, all configs
//! agreeing with it means all compiled outputs agree with each other — so any lever that changes
//! observable behavior is a DIVERGE, not a silent survivor. This is the net that would have
//! caught the rc-floor use-after-free that hid for ~2 days. (§1) GRAMMAR-COMPLETE GENERATOR: the
//! generator now emits USER FUNCTIONS with `let`/`own` params, closures, direct + mutual
//! recursion, tuple-of-owned-buffer returns, user ADTs with `match` / `if let` over heap
//! payloads, and — the class that hid the UAF — a local `var` that ALIAS-INITS a borrowed param
//! (or another local) and then SELF-REFERENTIALLY REASSIGNS it, with the shared value RE-READ
//! afterward as a use-after-free trip-wire. A grammar-coverage meta-assertion fails if any
//! statement kind was never generated.

use std::io::Write;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// Number of statement kinds the generator can emit (see `gen_program`'s match). The
/// grammar-coverage meta-assertion requires every one of these to appear across a run.
const NKINDS: u32 = 38;

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
        // Int arithmetic WRAPS on overflow (never traps), so full-range leaves stay total.
        match r.below(4) {
            0 => format!("({})", r.below(200) as i64 - 100),
            1 => format!("{}", r.next() % 1_000_000),
            2 => format!("({})", r.next() as i64),
            _ => format!("({})", i64::from_le_bytes(r.next().to_le_bytes())),
        }
    } else if r.below(6) < 5 {
        // Arithmetic. `+`/`-`/`*` WRAP on overflow (total). `/` and `%` are the only
        // int ops that TRAP — on a zero divisor or `INT_MIN / -1` — so those draw a
        // STRICTLY-POSITIVE divisor from `gen_pos_int`, keeping the generator TOTAL
        // (RFC-0058 §1, BUG-003): an unguarded divisor used to trap the whole program
        // before its first observable output, hiding value divergence behind agreement.
        match r.below(5) {
            0 => format!("({} + {})", gen_int(r, depth - 1), gen_int(r, depth - 1)),
            1 => format!("({} - {})", gen_int(r, depth - 1), gen_int(r, depth - 1)),
            2 => format!("({} * {})", gen_int(r, depth - 1), gen_int(r, depth - 1)),
            3 => format!("({} / {})", gen_int(r, depth - 1), gen_pos_int(r, depth - 1)),
            _ => format!("({} % {})", gen_int(r, depth - 1), gen_pos_int(r, depth - 1)),
        }
    } else {
        // `list.at` exercises indexed reads, but the index is CLAMPED in-bounds: a fresh
        // list literal of known length `n`, indexed in `0..n`. The read never traps, so
        // it yields a comparable value rather than an early trap that would discard the
        // rest of the program's output (RFC-0058 §1, BUG-003 — indices were unclamped).
        let n = 1 + r.below(4);
        let elems: Vec<String> = (0..n).map(|_| gen_int(r, depth - 1)).collect();
        let idx = r.below(n);
        format!("list.at([{}], {})", elems.join(", "), idx)
    }
}

/// A STRICTLY-POSITIVE, small, non-overflowing int expression — a safe `/`/`%` divisor
/// (never `0`, never `-1`, so neither div-by-zero nor the `INT_MIN / -1` trap can fire).
/// The only way `gen_int`'s division stays total (RFC-0058 §1). Each form is bounded
/// well below i64 so it cannot wrap negative through a `+`.
fn gen_pos_int(r: &mut Rng, depth: u32) -> String {
    match r.below(3) {
        0 => format!("{}", 1 + r.below(1000)),
        1 => format!("(1 + list.length({}))", gen_intlist(r, depth.min(1))),
        _ => format!("(1 + string.length({}))", gen_str(r, depth.min(1))),
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

/// `Some(int)` — exercises constructor/option value layout through interpolation.
/// Bare `None` can't infer its rendered type, so we stick to `Some`, which is the
/// heap-relevant case.
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
            // Substring over an IN-RANGE window (start 0, end in `0..=len`). Keeping the
            // indices valid makes the program total AND COMPARABLE: an out-of-range
            // substring is a VALUE the two backends clamp differently (a real codegen
            // parity divergence, tracked separately as a bug — not what this generator
            // is here to exercise, which is the heap-copy allocation path). Before the
            // BUG-003 totality fix this statement's divergence was masked by earlier traps.
            _ => {
                let s = gen_str(r, depth - 1);
                format!("string.substring({s}, 0, ({} % (string.length({s}) + 1)))", gen_pos_int(r, 0))
            }
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

/// A `Dict(String, Int)` built by a chain of insert/remove over a SMALL key space,
/// so inserts/removes/reinserts collide — the remove+reinsert+iterate pattern that has
/// previously corrupted the compiled dict. (`dict.new()` alone is ambiguous like `[]`, so
/// start from a typed insert.) RFC-0049 deleted `dict.set_at` (the literal `insert` alias),
/// so every upsert here is `insert`.
fn gen_dkey(r: &mut Rng) -> String {
    format!("\"k{}\"", r.below(4))
}

fn gen_dict(r: &mut Rng, ops: u32) -> String {
    let mut d = format!("dict_inserted(dict.new(), {}, {})", gen_dkey(r), gen_int(r, 1));
    for _ in 0..ops {
        d = match r.below(4) {
            1 => format!("dict_removed({}, {})", d, gen_dkey(r)),
            _ => format!("dict_inserted({}, {}, {})", d, gen_dkey(r), gen_int(r, 1)),
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

/// A random `Shape` ADT constructor. `Named` carries a COMPUTED (heap) string so the ADT's
/// heap-payload path — construction, `match` extraction, drop — is exercised, not just scalars.
fn gen_shape(r: &mut Rng) -> String {
    match r.below(3) {
        0 => format!("Circle({})", gen_int(r, 0)),
        1 => format!("Rect({}, {})", gen_int(r, 0), gen_int(r, 0)),
        _ => format!("Named(string.to_upper(\"{}\"))", alnum(r)),
    }
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
fn dict_inserted(d: Dict(String, Int), key: String, value: Int) -> Dict(String, Int):\n\
\x20   var out = d\n\
\x20   dict.insert(out, key, value)\n\
\x20   out\n\
fn dict_removed(d: Dict(String, Int), key: String) -> Dict(String, Int):\n\
\x20   var out = d\n\
\x20   dict.remove(out, key)\n\
\x20   out\n\
fn shape_area(sh: Shape) -> Int:\n\
\x20   match sh:\n\
\x20       Circle(rr) -> rr * rr\n\
\x20       Rect(w, h) -> w * h\n\
\x20       Named(nm) -> string.length(nm)\n\
fn grow(own xs: List(Int), n: Int) -> List(Int):\n\
\x20   var ys = xs\n\
\x20   var i = 0\n\
\x20   while i < n:\n\
\x20       list.push(ys, i)\n\
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
        "import string\nimport list\nimport math\nimport dict\nimport bytes\n\n\
         type R:\n    a: Int\n    b: String\n    c: List(Int)\n\
         type P:\n    x: R\n    y: Int\n\
         type Q:\n    m: Int\n    n: Int\n\
         type Q1:\n    a: Int\n\
         type Q3:\n    a: Int\n    b: Int\n    c: Int\n\
         type Shape:\n    Circle(Int)\n    Rect(Int, Int)\n    Named(String)\n\n",
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
            0 => format!("    console.print(\"${{{}}}\")\n", gen_int(&mut r, depth)),
            1 => format!("    console.print({})\n", gen_str(&mut r, depth)),
            2 => format!("    console.print(\"${{{}}}\")\n", gen_intlist(&mut r, 2)),
            3 => format!("    console.print(\"${{string.length({})}}\")\n", gen_str(&mut r, 2)),
            4 => format!("    console.print(\"${{list.length({})}}\")\n", gen_intlist(&mut r, 2)),
            5 => format!("    console.print(\"${{{}}}\")\n", gen_float(&mut r, depth)),
            6 => format!("    console.print(\"${{{}}}\")\n", gen_bool(&mut r, depth)),
            7 => format!("    console.print(\"${{{}}}\")\n", gen_option(&mut r)),
            8 => format!("    console.print(\"${{{}}}\")\n", gen_cond_int(&mut r, depth)),
            9 => format!("    console.print(\"${{{}}}\")\n", gen_strlist(&mut r, depth)),
            10 => format!("    console.print(\"${{{}}}\")\n", gen_nested_intlist(&mut r)),
            11 => format!("    console.print(\"${{{}}}\")\n", gen_tuple(&mut r)),
            12 => format!("    console.print(\"${{{}}}\")\n", gen_dict(&mut r, dops)),
            13 => format!("    console.print(\"${{dict.length({})}}\")\n", gen_dict(&mut r, dops)),
            14 => format!("    console.print(\"${{dict.pairs({})}}\")\n", gen_dict(&mut r, dops)),
            15 => {
                let d = gen_dict(&mut r, dops);
                let k = gen_dkey(&mut r);
                format!("    console.print(\"${{dict.get_or({}, {}, (-1))}}\")\n", d, k)
            }
            16 => format!("    console.print(\"${{{}}}\")\n", gen_record_r(&mut r)),
            17 => format!("    console.print(\"${{{}.a}}\")\n", gen_record_r(&mut r)),
            18 => format!("    console.print({}.b)\n", gen_record_r(&mut r)),
            19 => format!("    console.print(\"${{{}.c}}\")\n", gen_record_r(&mut r)),
            20 => format!("    console.print(\"${{{}}}\")\n", gen_record_p(&mut r)),
            21 => {
                // A confined `let`-bound list of a PACKABLE record read only via
                // `at(_).field` / `length` — the shape the packed `unbox` codegen flattens
                // into one flat buffer. VARYING the field count (1/2/3 scalar fields, via
                // Q1/Q/Q3) exercises different packed strides and offset math under the
                // checked heap. Index stays in-bounds (`below(m)`) so both backends agree.
                let m = 2 + r.below(3);
                let (ctor, fields): (&str, &[&str]) = match r.below(3) {
                    0 => ("Q1", &["a"]),
                    1 => ("Q", &["m", "n"]),
                    _ => ("Q3", &["a", "b", "c"]),
                };
                let elems: Vec<String> = (0..m)
                    .map(|_| {
                        let args: Vec<String> = fields.iter().map(|_| gen_int(&mut r, 1)).collect();
                        format!("{ctor}({})", args.join(", "))
                    })
                    .collect();
                let j = r.below(m);
                let reads: Vec<String> = fields.iter().map(|f| format!("list.at(qk{stmt_i}, {j}).{f}")).collect();
                format!(
                    "    let qk{stmt_i} = [{}]\n    console.print(\"${{{} + list.length(qk{stmt_i})}}\")\n",
                    elems.join(", "),
                    reads.join(" + "),
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
                    "    var rv{stmt_i} = [{}]\n    var rj{stmt_i} = 0\n    while rj{stmt_i} < {}:\n        rv{stmt_i} = [{}]\n        rj{stmt_i} = rj{stmt_i} + 1\n    console.print(\"${{list.at(rv{stmt_i}, 0) + list.length(rv{stmt_i})}}\")\n",
                    zeros,
                    iters,
                    elems.join(", "),
                )
            }
            23 => format!("    console.print(\"${{{}.x.a}}\")\n", gen_record_p(&mut r)),
            24 => {
                // Borrow a String into a helper that alias-inits + self-ref-reassigns it, then
                // RE-READ the shared arg afterward — the exact use-after-free trip-wire. Under a
                // bad free-at-overwrite the re-read of `sv` sees freed bytes and DIVERGES.
                let s = gen_str(&mut r, depth.min(2));
                format!(
                    "    let sv{stmt_i} = {s}\n    let av{stmt_i} = alias_str(sv{stmt_i})\n    console.print(sv{stmt_i})\n    console.print(av{stmt_i})\n"
                )
            }
            25 => {
                // Same trip-wire over a heap List: alias-init + self-ref concat inside the
                // helper, then re-read the borrowed list.
                let l = gen_intlist(&mut r, 2);
                format!(
                    "    let lv{stmt_i} = {l}\n    console.print(\"${{alias_list(lv{stmt_i})}}\")\n    console.print(\"${{lv{stmt_i}}}\")\n"
                )
            }
            26 => {
                // Alias a heap FIELD (`var b = rr.b`) then reassign the local; re-read the field.
                let rec = gen_record_r(&mut r);
                format!(
                    "    let fr{stmt_i} = {rec}\n    console.print(alias_field(fr{stmt_i}))\n    console.print(fr{stmt_i}.b)\n"
                )
            }
            27 => {
                // Statement-level confined alias-init + self-ref reassignment, re-reading the
                // original — the same class without a function boundary.
                let s1 = gen_str(&mut r, 1);
                let s2 = gen_str(&mut r, 1);
                format!(
                    "    var sc{stmt_i} = {s1}\n    sc{stmt_i} = sc{stmt_i} + {s2}\n    var tc{stmt_i} = sc{stmt_i}\n    tc{stmt_i} = string.to_upper(tc{stmt_i})\n    console.print(sc{stmt_i})\n    console.print(tc{stmt_i})\n"
                )
            }
            28 => {
                // `own`-buffer accumulator threaded through a loop (consumes a fresh literal).
                let l = gen_intlist(&mut r, 1);
                let n = r.below(5);
                format!("    console.print(\"${{grow({l}, {n})}}\")\n")
            }
            29 => {
                // Tuple of two OWNED buffers returned + destructured (the RFC-0036 executor shape).
                let a = gen_intlist(&mut r, 1);
                let b = gen_intlist(&mut r, 1);
                format!(
                    "    let (sp{stmt_i}, sq{stmt_i}) = swap2({a}, {b})\n    console.print(\"${{sp{stmt_i}}}\")\n    console.print(\"${{sq{stmt_i}}}\")\n"
                )
            }
            30 => {
                // Direct + mutual recursion (small bounded depth).
                let n1 = r.below(18);
                let n2 = r.below(18);
                format!("    console.print(\"${{accum({n1})}}\")\n    console.print(\"${{even_({n2})}}\")\n")
            }
            31 => {
                // Closure captured + applied through a function-typed parameter.
                let c = r.below(20) as i64 - 10;
                let x = r.below(20) as i64 - 10;
                format!(
                    "    let cl{stmt_i} = fn(z: Int) -> Int: z + ({c})\n    console.print(\"${{apply_twice(cl{stmt_i}, ({x}))}}\")\n"
                )
            }
            32 => {
                // Construct a `Shape` ADT (heap payload for `Named`) and `match` it in a helper.
                format!("    console.print(\"${{shape_area({})}}\")\n", gen_shape(&mut r))
            }
            33 => {
                // `match` as an EXPRESSION bound to a `let`, with indented arms binding payloads.
                format!(
                    "    let sh{stmt_i} = {}\n    let out{stmt_i} = match sh{stmt_i}:\n        Circle(rr) -> rr\n        Rect(w, h) -> w + h\n        Named(nm) -> string.length(nm)\n    console.print(\"${{out{stmt_i}}}\")\n",
                    gen_shape(&mut r)
                )
            }
            34 => {
                // `if let` binding a HEAP payload (`Named(nm)`) then re-reading it — extracts a
                // heap string out of an ADT and uses it past the destructure.
                format!(
                    "    let sh{stmt_i} = Named(string.to_upper(\"{}\"))\n    if let Named(nm{stmt_i}) = sh{stmt_i}:\n        console.print(nm{stmt_i})\n    else:\n        console.print(\"no\")\n",
                    alnum(&mut r)
                )
            }
            35 => {
                // A list of ADTs iterated + matched — heap-payload ADTs inside a list buffer.
                format!(
                    "    let shs{stmt_i} = [{}, {}, {}]\n    var acc{stmt_i} = 0\n    var jj{stmt_i} = 0\n    while jj{stmt_i} < 3:\n        acc{stmt_i} = acc{stmt_i} + shape_area(shs{stmt_i}[jj{stmt_i}])\n        jj{stmt_i} = jj{stmt_i} + 1\n    console.print(\"${{acc{stmt_i}}}\")\n",
                    gen_shape(&mut r),
                    gen_shape(&mut r),
                    gen_shape(&mut r)
                )
            }
            36 => {
                // Bytes share String's flat heap layout but use byte-oriented operations.
                // Round-trip a computed string so both construction and decoding allocate.
                format!(
                    "    console.print(bytes.to_string(bytes.from_string({})))\n",
                    gen_str(&mut r, depth.min(2))
                )
            }
            37 => {
                // Exercise byte concat/slice plus alias stability: the original buffers must
                // remain readable after constructing and slicing a joined value.
                let a = gen_str(&mut r, depth.min(2));
                let b = gen_str(&mut r, depth.min(2));
                format!(
                    "    let ba{stmt_i} = bytes.from_string({a})\n    let bb{stmt_i} = bytes.from_string({b})\n    let bc{stmt_i} = bytes.concat(ba{stmt_i}, bb{stmt_i})\n    console.print(\"${{bytes.to_list(bytes.slice(bc{stmt_i}, (-2), bytes.length(bc{stmt_i}) + 2))}}\")\n    console.print(\"${{bytes.to_list(ba{stmt_i})}}\")\n    console.print(\"${{bytes.to_list(bb{stmt_i})}}\")\n"
                )
            }
            _ => unreachable!("kind is sampled below NKINDS"),
        };
        body.push_str(&line);
    }
    (body, used)
}

/// The cross-lever config set (RFC-0037 §2). Every program is run under each: the baseline
/// (`none`, all opts off), the production default (``), each high-risk pass ALONE from a
/// `none` base (`rc-floor`, `unbox`, `closure-elide` — the class that hid the UAF is exercised
/// in isolation, not just masked inside the union), and the union (`all,-wasm-opt`, which omits
/// the external Binaryen post-pass). `closure-elide` (RFC-0062) elides a non-escaping closure's heap
/// environment: a wrong non-escape classification would drop an env a call still reads, so it
/// is swept both alone and in the union.
const CONFIGS: &[&str] = &[
    "none",
    "",
    "none,rc-floor",
    "none,unbox",
    "none,closure-elide",
    "all,-wasm-opt",
];

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A process-and-probe-unique temp path (BUG-010). Fixed names in the shared temp dir
/// let two concurrent harness runs (two fuzz jobs, or a fuzz job next to a dev run —
/// concurrent agents are the norm here) interleave writes and compute a verdict for a
/// DIFFERENT program than the one the harness believes it is testing. PID + a monotonic
/// counter + nanos makes every invocation AND every shrink probe distinct.
fn unique_temp_path(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("witchy_{prefix}_{}_{n}_{nanos}.witchy", std::process::id()))
}

/// The per-program wall-clock budget (RFC-0058 §3). A generated program that runs longer
/// than this is a hang (or a pathological blow-up) — a bug, never silently "agree".
fn fuzz_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(env_usize("WITCHY_FUZZ_TIMEOUT_SECS", 60) as u64)
}

/// Shrink probes inherit a SMALLER budget (RFC-0058 §3) — the minimizer runs many probes
/// on the rare failure path, and a minimized case should reproduce fast.
fn shrink_timeout() -> std::time::Duration {
    fuzz_timeout().min(std::time::Duration::from_secs(10))
}

/// Run `cmd` with a wall-clock budget. Returns `Some(output)` if it finished in time,
/// `None` if it exceeded `timeout` (the child is killed) — the distinct `timed-out`
/// class (RFC-0058 §3) so a hung program is never miscounted as agreement. Output is
/// bounded (a few KB), so the poll-then-collect pattern cannot deadlock on a full pipe.
fn run_with_timeout(cmd: &mut Command, timeout: std::time::Duration) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Parse the machine-readable `parity-stats outcome=<...> compared=<N> ...` line
/// (RFC-0058 §2) into `(outcome, compared)`. Consumers branch on THIS + the exit code,
/// never on the human `DIVERGE` text — the BUG-002 fix at the source.
fn parse_stats(stdout: &str) -> Option<(String, usize)> {
    let line = stdout.lines().rev().find(|l| l.starts_with("parity-stats "))?;
    let mut outcome = None;
    let mut compared = None;
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("outcome=") {
            outcome = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("compared=") {
            compared = v.parse().ok();
        }
    }
    Some((outcome?, compared?))
}

/// Outcome of running `witchy parity` on one program under one lever. Mirrors the CLI's
/// four mechanical outcomes (RFC-0058 §2) plus the harness-side `Crash`/`TimedOut` classes.
enum ParityResult {
    /// Both backends produced equal output — carries the compared line count.
    Agree(usize),
    /// Both backends errored and agree (0 compared lines — pulls the median down).
    BothErrorAgree,
    /// `unexpected-error`: a non-compiling generated program (a tolerated generator miss).
    Skip,
    Diverge(String),
    Crash(String),
    /// The run exceeded its wall-clock budget (RFC-0058 §3) — never counted as agreement.
    TimedOut,
}

/// Run `witchy parity` on `src` under `WITCHY_OPT=cfg`, in a unique temp file, with the
/// default per-program timeout. Classifies by EXIT CODE + the machine-readable stats line.
fn run_parity(src: &str, cfg: &str, prefix: &str) -> ParityResult {
    run_parity_t(src, cfg, prefix, fuzz_timeout())
}

/// `run_parity` with an explicit timeout (shrink probes pass a smaller budget).
fn run_parity_t(src: &str, cfg: &str, prefix: &str, timeout: std::time::Duration) -> ParityResult {
    let path = unique_temp_path(prefix);
    std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.args(["parity", path.to_str().unwrap()]).env("WITCHY_OPT", cfg);
    let out = run_with_timeout(&mut cmd, timeout);
    let _ = std::fs::remove_file(&path);
    let Some(out) = out else {
        return ParityResult::TimedOut;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // No exit code = terminated by signal = the host process itself died (memory unsafety).
    let Some(code) = out.status.code() else {
        return ParityResult::Crash(format!("{stdout}{stderr}"));
    };
    // Classify on the CLI's exit-code taxonomy (RFC-0058 §2): 0 = a pass (agree or
    // both-error-agree, disambiguated by the stats line), 3 = diverge, 2 = a compile
    // miss (Skip). Anything else is unexpected and treated as a host-level anomaly.
    match code {
        0 => match parse_stats(&stdout) {
            Some((outcome, _)) if outcome == "both-error-agree" => ParityResult::BothErrorAgree,
            Some((_, compared)) => ParityResult::Agree(compared),
            None => ParityResult::Crash(format!("parity exit 0 without a stats line\n{stdout}{stderr}")),
        },
        3 => ParityResult::Diverge(format!("{stdout}{stderr}")),
        2 => ParityResult::Skip,
        other => ParityResult::Crash(format!("unexpected parity exit code {other}\n{stdout}{stderr}")),
    }
}

/// Whether `witchy check` accepts `src`. The differential fuzzer may skip sources
/// that fail the public acceptance gate, but a source that passes `check` must run
/// on both backends; otherwise the acceptance set has drifted.
fn check_accepts(src: &str, prefix: &str) -> Result<bool, String> {
    let path = unique_temp_path(prefix);
    std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
    let out = Command::new(BIN).args(["check", path.to_str().unwrap()]).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match out.status.code() {
        Some(0) => Ok(true),
        Some(_) => Ok(false),
        None => Err(format!("{stdout}{stderr}")),
    }
}

/// (RFC-0037 §6) Greedily minimize a failing program: drop body lines one at a time, keeping any
/// drop under which `fails` still holds, to a fixpoint. A structural line whose removal stops the
/// failure (an import, a `let` a later line needs) is kept automatically — the predicate returns
/// false for it. Bounded by `budget` predicate calls so the (rare) failure path stays finite.
/// Standard line-level delta debugging; converges to a minimal reproducer for the report.
fn shrink<F: Fn(&str) -> bool>(src: &str, fails: F, mut budget: usize) -> String {
    let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut changed = true;
    while changed && budget > 0 {
        changed = false;
        let mut i = lines.len();
        while i > 0 && budget > 0 {
            i -= 1;
            let mut cand = lines.clone();
            cand.remove(i);
            let text = format!("{}\n", cand.join("\n"));
            budget -= 1;
            if fails(&text) {
                lines = cand;
                changed = true;
            }
        }
    }
    format!("{}\n", lines.join("\n"))
}

/// A DIVERGE or a host crash — the two failure outcomes that `shrink` preserves while minimizing.
fn is_failure(r: &ParityResult) -> bool {
    matches!(r, ParityResult::Diverge(_) | ParityResult::Crash(_))
}

#[test]
fn shrink_reduces_to_minimal_repro() {
    // Synthetic failure oracle: a program "fails" iff a marker line is present. The minimizer
    // must drop every other line, converging to just the marker — proving the delta-debug loop.
    let src = "line a\nline b\nMARKER here\nline c\nline d\n";
    let min = shrink(src, |s| s.contains("MARKER here"), 1000);
    let kept: Vec<&str> = min.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(kept, vec!["MARKER here"], "shrink did not reduce to the minimal failing line: {min:?}");
}

/// Per-optimizer-config outcome tally (RFC-0058 §5) — the full matrix, not the old
/// aggregate that only counted the empty default. Diverge/Crash/TimedOut are not tallied
/// here: they panic immediately (the run has already failed).
#[derive(Default, Debug)]
struct ConfigTally {
    agree: usize,
    both_error: usize,
    skip: usize,
    compared_lines: usize,
}

#[test]
fn differential_fuzz_interpreter_vs_compiled() {
    // Counts are env-overridable so the scheduled/`--full` job can scale coverage up. The
    // defaults keep total work (programs × statements × CONFIGS) close to the pre-cross-lever
    // budget while adding per-lever coverage.
    let programs = env_usize("WITCHY_FUZZ_PROGRAMS", 30);
    let statements = env_usize("WITCHY_FUZZ_STATEMENTS", 100);
    let default_idx = CONFIGS.iter().position(|c| c.is_empty()).unwrap();

    struct SeedResult {
        kinds_used: u64,
        tallies: Vec<ConfigTally>,
        default_compared: usize,
    }

    // Run seeds in parallel — each is independent (spawns its own subprocesses).
    // Panics (DIVERGE/Crash/Timeout) propagate naturally via thread::scope.
    let results: Vec<SeedResult> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..programs as u64).map(|seed| {
            s.spawn(move || {
                let (src, used) = gen_program(seed.wrapping_mul(0x1234_5678_9ABC_DEF1).wrapping_add(1), statements);
                let check_ok = check_accepts(&src, &format!("check_s{seed}")).unwrap_or_else(|out| {
                    panic!("witchy check CRASHED (signal) on generated seed {seed}.\n--- program ---\n{src}\n--- output ---\n{out}")
                });
                let mut seed_tallies: Vec<ConfigTally> = (0..CONFIGS.len()).map(|_| ConfigTally::default()).collect();
                let mut seed_outcomes: Vec<&'static str> = Vec::with_capacity(CONFIGS.len());
                let mut default_compared: usize = 0;
                for (ci, &cfg) in CONFIGS.iter().enumerate() {
                    let label = if cfg.is_empty() { "<default>" } else { cfg };
                    match run_parity(&src, cfg, &format!("s{seed}c{ci}")) {
                        ParityResult::Crash(out) => {
                            let min = shrink(&src, |s| is_failure(&run_parity_t(s, cfg, "shrink", shrink_timeout())), 4000);
                            panic!(
                                "witchy CRASHED (signal) on seed {seed} under WITCHY_OPT={label} — host-level memory unsafety.\n--- minimal repro ({} lines, from {}) ---\n{min}--- output ---\n{out}",
                                min.lines().count(),
                                src.lines().count()
                            );
                        }
                        ParityResult::Diverge(out) => {
                            let min = shrink(&src, |s| is_failure(&run_parity_t(s, cfg, "shrink", shrink_timeout())), 4000);
                            panic!(
                                "BACKENDS DIVERGE on seed {seed} under WITCHY_OPT={label} — an optimization changed observable behavior.\n--- minimal repro ({} lines, from {}) ---\n{min}--- output ---\n{out}",
                                min.lines().count(),
                                src.lines().count()
                            );
                        }
                        ParityResult::TimedOut => panic!(
                            "witchy parity TIMED OUT on seed {seed} under WITCHY_OPT={label} after {}s — a generated program hung (a bug; or raise WITCHY_FUZZ_TIMEOUT_SECS if legitimately slow).",
                            fuzz_timeout().as_secs()
                        ),
                        ParityResult::Agree(n) => {
                            seed_tallies[ci].agree += 1;
                            seed_tallies[ci].compared_lines += n;
                            seed_outcomes.push("agree");
                            if cfg.is_empty() { default_compared = n; }
                        }
                        ParityResult::BothErrorAgree => {
                            seed_tallies[ci].both_error += 1;
                            seed_outcomes.push("both-error");
                        }
                        ParityResult::Skip => {
                            assert!(
                                !check_ok,
                                "`witchy check` accepted generated seed {seed}, but parity skipped it under WITCHY_OPT={label} — check and the compiled backend have different acceptance sets.\n--- program ---\n{src}"
                            );
                            seed_tallies[ci].skip += 1;
                            seed_outcomes.push("skip");
                        }
                    }
                }
                // Cross-config consistency (RFC-0058 §5).
                if seed_outcomes[default_idx] == "agree" {
                    for (ci, &cfg) in CONFIGS.iter().enumerate() {
                        assert_ne!(
                            seed_outcomes[ci], "skip",
                            "config WITCHY_OPT={} SKIPPED seed {seed} that the default config AGREED on — a lever changed compilability (RFC-0058 §5).",
                            if cfg.is_empty() { "<default>" } else { cfg }
                        );
                    }
                }
                SeedResult { kinds_used: used, tallies: seed_tallies, default_compared }
            })
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Reduce per-seed results.
    let mut tallies: Vec<ConfigTally> = (0..CONFIGS.len()).map(|_| ConfigTally::default()).collect();
    let mut kinds_used: u64 = 0;
    let mut default_compared: Vec<usize> = Vec::with_capacity(programs);
    for r in &results {
        kinds_used |= r.kinds_used;
        default_compared.push(r.default_compared);
        for (ci, t) in r.tallies.iter().enumerate() {
            tallies[ci].agree += t.agree;
            tallies[ci].both_error += t.both_error;
            tallies[ci].skip += t.skip;
            tallies[ci].compared_lines += t.compared_lines;
        }
    }

    let labeled: Vec<(&str, &ConfigTally)> = CONFIGS
        .iter()
        .map(|c| if c.is_empty() { "<default>" } else { *c })
        .zip(tallies.iter())
        .collect();
    // Sanity: the generator must mostly produce compiling programs, or it isn't testing codegen.
    let default_tally = &tallies[default_idx];
    assert!(
        default_tally.agree * 2 >= programs,
        "fuzzer mostly produced non-compiling programs ({} agree, {} both-error, {} skipped of {programs}) — fix the generator",
        default_tally.agree,
        default_tally.both_error,
        default_tally.skip
    );
    // Execution-volume vacuity guard (RFC-0058 §1/§4, BUG-003): the MEDIAN compared-line
    // count over the default config must be >= 1, so a corpus that traps before its first
    // observable effect fails LOUDLY instead of passing vacuously — the exact failure mode
    // that made the old both-error-counts-as-agree accounting blind to value divergence.
    default_compared.sort_unstable();
    let median = default_compared.get(default_compared.len() / 2).copied().unwrap_or(0);
    assert!(
        median >= 1,
        "differential fuzz VACUOUS: median compared-line count is {median} (< 1) over {programs} programs — the generator traps before producing comparable output (BUG-003). Per-config tallies: {labeled:?}"
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
            "generator never emitted statement kind(s) (missing bitmask {missing:#b}) — a grammar-coverage hole; raise counts or fix the generator"
        );
    }
    eprintln!(
        "differential fuzz: {programs} programs × {} configs; default median compared-lines {median}; per-config tallies {labeled:?}; kinds covered {kinds_used:#040b}",
        CONFIGS.len()
    );
}

/// (RFC-0037 §3) The use-after-free sanitizer (`WITCHY_UAF_CHECK=1`) poisons every freed block
/// so a stale read of an un-reused block reads a trap pattern — a deterministic DIVERGE where
/// the plain differential can miss (a freed block is only *reliably* corrupted through its
/// offset-0 freelist link; a stale read at offset 4.. of an un-reused block is otherwise
/// intact). The sanitizer is STRICTLY ADDITIVE: on a CORRECT compiler a freed block is never
/// read again, so poisoning it changes no output. This test enforces that zero-false-positive
/// property over generated programs AND exercises the poison-on-free path every run, so a
/// regression in the sanitizer itself (a bad size, an out-of-bounds poison store) surfaces as a
/// host crash or a spurious DIVERGE. Its BUG-CATCHING value is realized by the same net the
/// moment a real reclamation bug lands under `rc-floor`.
/// (RFC-0037 §4) A program asserting algebraic stdlib laws over random data. Each law prints a
/// single `true`/`false`. Checking the printed value — not just backend agreement — is the point:
/// a law that is `false` on BOTH backends passes the differential (they agree) yet is a real bug,
/// so this is the one net that catches an oracle that is *itself* wrong (gap G4). The laws are
/// fixed and total (no trapping inputs), so a well-formed program always results.
fn gen_law_program(seed: u64) -> String {
    let mut r = Rng(seed);
    let ilist = |r: &mut Rng| {
        let n = 1 + r.below(5);
        let e: Vec<String> = (0..n).map(|_| format!("{}", r.below(50) as i64 - 25)).collect();
        format!("[{}]", e.join(", "))
    };
    let xs = ilist(&mut r);
    let a = ilist(&mut r);
    let b = ilist(&mut r);
    let s1 = format!("\"{}\"", alnum(&mut r));
    let s2 = format!("\"{}\"", alnum(&mut r));
    let k = gen_dkey(&mut r);
    let v = format!("{}", r.below(1000));
    let v2 = format!("{}", r.below(1000));
    let rep = r.below(4);
    // Helper predicates (they print nothing). A CORRECT sort is BOTH sorted AND a
    // permutation of its input — laws the old idempotence + length pair could NOT express
    // (that pair is satisfied by `list.sort = identity`). `is_perm` compares element
    // multiplicities (RFC-0058 §6), so a lossy/duplicating "sort" that stays the right
    // length is now caught, and `is_sorted` kills identity on any unsorted input.
    let helpers = "\
fn count_eq(xs: List(Int), target: Int) -> Int:\n\
\x20   var c = 0\n\
\x20   for x in xs:\n\
\x20       if x == target:\n\
\x20           c = c + 1\n\
\x20   c\n\
fn is_perm(a: List(Int), b: List(Int)) -> Bool:\n\
\x20   var ok = list.length(a) == list.length(b)\n\
\x20   for v in a:\n\
\x20       if count_eq(a, v) != count_eq(b, v):\n\
\x20           ok = false\n\
\x20   ok\n\
fn is_sorted(xs: List(Int)) -> Bool:\n\
\x20   var ok = true\n\
\x20   var i = 1\n\
\x20   while i < list.length(xs):\n\
\x20       if list.at(xs, i) < list.at(xs, i - 1):\n\
\x20           ok = false\n\
\x20       i = i + 1\n\
\x20   ok\n\
fn reversed(xs: List(Int)) -> List(Int):\n\
\x20   var out = xs\n\
\x20   list.reverse(out)\n\
\x20   out\n\
fn sorted(xs: List(Int)) -> List(Int):\n\
\x20   var out = xs\n\
\x20   list.sort(out)\n\
\x20   out\n\
fn dict_inserted(d: Dict(String, Int), key: String, value: Int) -> Dict(String, Int):\n\
\x20   var out = d\n\
\x20   dict.insert(out, key, value)\n\
\x20   out\n\
fn dict_removed(d: Dict(String, Int), key: String) -> Dict(String, Int):\n\
\x20   var out = d\n\
\x20   dict.remove(out, key)\n\
\x20   out\n";
    format!(
        "import list\nimport string\nimport dict\nimport bytes\nimport set\n\n{helpers}\nfn main(console: Console):\n\
         \x20   let xs = {xs}\n\
         \x20   let a = {a}\n\
         \x20   let b = {b}\n\
         \x20   let s1 = {s1}\n\
         \x20   let s2 = {s2}\n\
         \x20   let d = dict_inserted(dict.new(), \"seed\", 1)\n\
         \x20   let d2 = dict_inserted(dict_inserted(dict.new(), {s1}, {v}), {s2}, {v2})\n\
         \x20   let rt = dict_inserted(dict_removed(d2, {s1}), {s1}, {v})\n\
         \x20   let sa = set.from_list(a)\n\
         \x20   let sb = set.from_list(b)\n\
         \x20   let ba = bytes.from_string(s1)\n\
         \x20   let bb = bytes.from_string(s2)\n\
         \x20   console.print(\"${{reversed(reversed(xs)) == xs}}\")\n\
         \x20   console.print(\"${{list.length(list.concat(a, b)) == list.length(a) + list.length(b)}}\")\n\
         \x20   console.print(\"${{sorted(sorted(xs)) == sorted(xs)}}\")\n\
         \x20   console.print(\"${{list.length(sorted(xs)) == list.length(xs)}}\")\n\
         \x20   console.print(\"${{is_sorted(sorted(xs))}}\")\n\
         \x20   console.print(\"${{is_perm(sorted(xs), xs)}}\")\n\
         \x20   console.print(\"${{dict.get_or(dict_inserted(d, {k}, {v}), {k}, 0 - 1) == {v}}}\")\n\
         \x20   console.print(\"${{(dict.get_or(rt, {s1}, 0 - 1) == {v}) && (dict.length(rt) == dict.length(d2)) && (list.length(dict.pairs(rt)) == dict.length(rt))}}\")\n\
         \x20   console.print(\"${{string.length(s1 + s2) == string.length(s1) + string.length(s2)}}\")\n\
         \x20   console.print(\"${{string.reverse(string.reverse(s1)) == s1}}\")\n\
         \x20   console.print(\"${{string.length(string.repeat(s1, {rep})) == string.length(s1) * {rep}}}\")\n\
         \x20   console.print(\"${{bytes.to_string(ba) == s1}}\")\n\
         \x20   console.print(\"${{bytes.to_list(bytes.concat(ba, bb)) == list.concat(bytes.to_list(ba), bytes.to_list(bb))}}\")\n\
         \x20   console.print(\"${{set.union(sa, sb) == set.union(sb, sa)}}\")\n\
         \x20   console.print(\"${{set.is_subset(set.intersection(sa, sb), sa) && set.is_subset(set.intersection(sa, sb), sb)}}\")\n\
         \x20   console.print(\"${{set.length(set.from_list(list.concat(a, a))) == set.length(sa)}}\")\n"
    )
}

/// Number of algebraic laws `gen_law_program` prints — used to assert none were skipped by an
/// early trap (which would otherwise slip through as "fewer lines, all true"). The list laws
/// include sortedness + permutation, dict remove/reinsert/iterate, byte-buffer round-trips,
/// and set algebra (§6).
const NLAWS: usize = 16;

/// A minimal helper library for the dead-alloc metamorphic pair: the two alias/self-ref shapes
/// whose reclamation is sensitive to free-list state (no `type R` dependency, unlike `HELPER_LIB`).
const HELPER_LIB_MINI: &str = "\
fn alias_str(s: String) -> String:\n\
\x20   var t = s\n\
\x20   t = t + \"!\"\n\
\x20   t\n\
fn alias_list(xs: List(Int)) -> List(Int):\n\
\x20   var ys = xs\n\
\x20   ys = list.concat(ys, ys)\n\
\x20   ys\n";

/// (RFC-0037 §4, semantics-preserving transform) A base program of alias/self-ref units and a
/// TWIN that interleaves DEAD (unused) heap allocations. The dead bindings cannot change the
/// program's meaning, so base and twin must print identically — but they DO change the
/// allocation order and free-list state, so if a reclamation/aliasing bug lets a freed block be
/// reused differently between the two, the outputs diverge. This catches the fragile
/// use-after-free class (a freed block reused-and-overwritten in one variant but not the other)
/// in the DEFAULT path, where the `WITCHY_UAF_CHECK` sanitizer's poison is not present. Returns
/// `(base, twin)` sharing the same helper library so only the dead bindings differ.
fn gen_reclaim_pair(seed: u64) -> (String, String) {
    let mut r = Rng(seed);
    let units = 6 + r.below(6);
    let mut base = String::new();
    let mut twin = String::new();
    for i in 0..units {
        // dead allocations — ONLY in the twin — perturb the free-list without changing semantics.
        let dk = 1 + r.below(6);
        let de: Vec<String> = (0..dk).map(|_| format!("{}", r.below(20))).collect();
        twin.push_str(&format!(
            "    let dz{i} = [{}]\n    let dw{i} = string.to_upper(\"{}\")\n",
            de.join(", "),
            alnum(&mut r)
        ));
        // a shared heap value aliased then RE-READ (the fragile use-after-free class); heap
        // strings/lists (computed, not static literals) so the rc machinery is live.
        let unit = if r.below(2) == 0 {
            format!(
                "    let sv{i} = string.to_upper(\"{}\")\n    let av{i} = alias_str(sv{i})\n    console.print(sv{i})\n    console.print(av{i})\n",
                alnum(&mut r)
            )
        } else {
            let n = 1 + r.below(5);
            let e: Vec<String> = (0..n).map(|_| format!("{}", r.below(50))).collect();
            format!(
                "    let lv{i} = [{}]\n    console.print(\"${{alias_list(lv{i})}}\")\n    console.print(\"${{lv{i}}}\")\n",
                e.join(", ")
            )
        };
        base.push_str(&unit);
        twin.push_str(&unit);
    }
    let header = format!("import list\nimport string\n\n{HELPER_LIB_MINI}\nfn main(console: Console):\n");
    (format!("{header}{base}"), format!("{header}{twin}"))
}

/// Compile+run `src` (compiled backend, `witchy <file>`) under `WITCHY_OPT=cfg`; return
/// `(exit_ok, stdout)`. A trap and a value are distinguished by `exit_ok`, so a UAF that traps
/// in one variant but succeeds in the other is a mismatch.
fn run_compiled(src: &str, cfg: &str, tag: &str) -> (bool, String) {
    let path = unique_temp_path(&format!("reclaim_{tag}"));
    std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
    let out = Command::new(BIN).arg(path.to_str().unwrap()).env("WITCHY_OPT", cfg).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.code().is_some(), "witchy crashed (signal) running a metamorphic variant.\n{src}");
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn metamorphic_dead_alloc_invariant() {
    let programs = env_usize("WITCHY_RECLAIM_PROGRAMS", 30);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..programs as u64).map(|seed| {
            s.spawn(move || {
                let (base, twin) = gen_reclaim_pair(seed.wrapping_mul(0xD1B5_4A32_D192_ED03).wrapping_add(3));
                for cfg in ["", "rc-floor"] {
                    let b = run_compiled(&base, cfg, &format!("{seed}_base"));
                    let t = run_compiled(&twin, cfg, &format!("{seed}_twin"));
                    assert_eq!(
                        b, t,
                        "dead-alloc metamorphic FAILED under WITCHY_OPT={cfg:?} on seed {seed}: inserting unused allocations changed observable behavior (a reclamation/aliasing bug).\n--- base ---\n{base}\n--- twin ---\n{twin}"
                    );
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    });
}

#[test]
fn metamorphic_property_laws() {
    let programs = env_usize("WITCHY_LAW_PROGRAMS", 40);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..programs as u64).map(|seed| {
            s.spawn(move || {
                let src = gen_law_program(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7));
                let path = unique_temp_path(&format!("law_{seed}"));
                std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
                let par = Command::new(BIN).args(["parity", path.to_str().unwrap()]).output().unwrap();
                let pout = String::from_utf8_lossy(&par.stdout);
                let perr = String::from_utf8_lossy(&par.stderr);
                assert!(
                    par.status.code().is_some(),
                    "witchy crashed (signal) on law seed {seed}.\n--- program ---\n{src}\n{perr}"
                );
                if pout.contains("DIVERGE") || perr.contains("DIVERGE") {
                    panic!("BACKENDS DIVERGE on law seed {seed}.\n--- program ---\n{src}\n--- output ---\n{pout}{perr}");
                }
                assert!(par.status.success(), "law program failed to compile on seed {seed}.\n--- program ---\n{src}\n{pout}{perr}");
                let run = Command::new(BIN).arg(path.to_str().unwrap()).output().unwrap();
                let _ = std::fs::remove_file(&path);
                let rout = String::from_utf8_lossy(&run.stdout);
                let lines: Vec<&str> = rout.lines().filter(|l| !l.is_empty()).collect();
                assert_eq!(
                    lines.len(),
                    NLAWS,
                    "law seed {seed}: expected {NLAWS} law results, got {} (an early trap?).\n--- program ---\n{src}\n--- output ---\n{rout}",
                    lines.len()
                );
                for (i, line) in lines.iter().enumerate() {
                    assert_eq!(
                        *line, "true",
                        "algebraic LAW #{i} VIOLATED on seed {seed} (printed {line:?}) — a bug even though the backends agree.\n--- program ---\n{src}"
                    );
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    });
}

#[test]
fn uaf_sanitizer_is_false_positive_free() {
    let programs = env_usize("WITCHY_UAF_FUZZ_PROGRAMS", 12);
    let statements = env_usize("WITCHY_UAF_FUZZ_STATEMENTS", 100);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..programs as u64).map(|seed| {
            s.spawn(move || {
                let (src, _) = gen_program(seed.wrapping_mul(0x1234_5678_9ABC_DEF1).wrapping_add(1), statements);
                let path = unique_temp_path(&format!("uaf_{seed}"));
                std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
                let out = Command::new(BIN)
                    .args(["parity", path.to_str().unwrap()])
                    .env("WITCHY_OPT", "rc-floor")
                    .env("WITCHY_UAF_CHECK", "1")
                    .output()
                    .unwrap();
                let _ = std::fs::remove_file(&path);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    out.status.code().is_some(),
                    "witchy crashed (signal) on seed {seed} under the UAF sanitizer — a bad poison store.\n--- program ---\n{src}\n--- stderr ---\n{stderr}"
                );
                if stdout.contains("DIVERGE") || stderr.contains("DIVERGE") {
                    panic!(
                        "UAF sanitizer FALSE POSITIVE on seed {seed}: a correct compiler diverged under WITCHY_UAF_CHECK=1 (poisoning a freed block must never change output).\n--- program ---\n{src}\n--- output ---\n{stdout}{stderr}"
                    );
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    });
}

/// (RFC-0051 I1 step 3) The dup/drop assertion sweep — the fire-and-report backstop's
/// zero-fires tracking gate. Under `WITCHY_RC_ASSERT=1` a value that reaches
/// `$rc_dup`/`$rc_drop` at/above `heap_base` with an IMPLAUSIBLE header traps instead of
/// silently skipping — exactly an I1 emission-invariant violation (codegen dup'd/dropped
/// a NON-owning value: a view/slice/scalar). The type predicates
/// (`list_elem_is_offset0_rc` / `expr_is_offset0_rc`) are meant to be the sole gate; this
/// sweep is the evidence they hold across the random corpus. A trap here (DIVERGE: the
/// interp succeeds, the compiled backend hits `unreachable`) names a real predicate gap —
/// the SEC-037 class. Zero fires across this + examples + e2e is the RFC's precondition
/// for deleting the release-path `header_ok` heuristic entirely. Runs under `rc-floor`
/// (the only lever that emits dup/drop).
#[test]
fn rc_assert_dup_drop_is_false_positive_free() {
    let programs = env_usize("WITCHY_RC_ASSERT_PROGRAMS", 12);
    let statements = env_usize("WITCHY_RC_ASSERT_STATEMENTS", 100);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..programs as u64).map(|seed| {
            s.spawn(move || {
                let (src, _) = gen_program(seed.wrapping_mul(0x0F1E_2D3C_4B5A_6978).wrapping_add(7), statements);
                let path = unique_temp_path(&format!("rcassert_{seed}"));
                std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
                let out = Command::new(BIN)
                    .args(["parity", path.to_str().unwrap()])
                    .env("WITCHY_OPT", "rc-floor")
                    .env("WITCHY_RC_ASSERT", "1")
                    .output()
                    .unwrap();
                let _ = std::fs::remove_file(&path);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(
                    out.status.code().is_some(),
                    "witchy crashed (signal) on seed {seed} under the RC assertion.\n--- program ---\n{src}\n--- stderr ---\n{stderr}"
                );
                if stdout.contains("DIVERGE") || stderr.contains("DIVERGE") {
                    panic!(
                        "RC-ASSERT I1 VIOLATION on seed {seed}: codegen emitted a dup/drop on a value with an implausible header (a view/slice/scalar reached a count op) under WITCHY_RC_ASSERT=1 — the type predicate is not airtight for this shape.\n--- program ---\n{src}\n--- output ---\n{stdout}{stderr}"
                    );
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    });
}
