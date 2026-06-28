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

use std::io::Write;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

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
    if depth == 0 || r.below(3) == 0 {
        // Spread magnitudes: small signed, mid, and full-range i64 (renders near page edges).
        match r.below(4) {
            0 => format!("({})", r.below(200) as i64 - 100),
            1 => format!("{}", r.next() % 1_000_000),
            2 => format!("({})", r.next() as i64),
            _ => format!("({})", i64::from_le_bytes(r.next().to_le_bytes())),
        }
    } else {
        let op = ["+", "-", "*"][r.below(3) as usize];
        format!("({} {} {})", gen_int(r, depth - 1), op, gen_int(r, depth - 1))
    }
}

fn gen_str(r: &mut Rng, depth: u32) -> String {
    if depth == 0 || r.below(3) == 0 {
        match r.below(3) {
            0 => format!("\"{}\"", alnum(r)),
            1 => format!("\"x${{{}}}y\"", gen_int(r, depth.min(2))), // `${int}` -> int_to_string
            _ => "\"\"".to_string(),
        }
    } else {
        format!("({} + {})", gen_str(r, depth - 1), gen_str(r, depth - 1))
    }
}

fn gen_intlist(r: &mut Rng, depth: u32) -> String {
    let n = r.below(5);
    let elems: Vec<String> = (0..n).map(|_| gen_int(r, depth.min(2))).collect();
    format!("[{}]", elems.join(", "))
}

/// One random program: a `main` that prints many heap-exercising expressions.
fn gen_program(seed: u64, statements: usize) -> String {
    let mut r = Rng(seed);
    let mut body = String::from("import string\nimport list\n\nfn main(console: Console):\n");
    for _ in 0..statements {
        let kind = r.below(5);
        let depth = 1 + r.below(3) as u32;
        let line = match kind {
            0 => format!("    print(console, __render({}))\n", gen_int(&mut r, depth)),
            1 => format!("    print(console, {})\n", gen_str(&mut r, depth)),
            2 => format!("    print(console, __render({}))\n", gen_intlist(&mut r, 2)),
            3 => format!("    print(console, \"${{string.length({})}}\")\n", gen_str(&mut r, 2)),
            _ => format!("    print(console, \"${{list.length({})}}\")\n", gen_intlist(&mut r, 2)),
        };
        body.push_str(&line);
    }
    body
}

#[test]
fn differential_fuzz_interpreter_vs_compiled() {
    let programs = 40usize;
    let statements = 120usize;
    let mut agree = 0;
    let mut compile_skipped = 0;
    for seed in 0..programs as u64 {
        let src = gen_program(seed.wrapping_mul(0x1234_5678_9ABC_DEF1).wrapping_add(1), statements);
        let path = std::env::temp_dir().join(format!("witchy_fuzz_{seed}.witchy"));
        std::fs::File::create(&path).unwrap().write_all(src.as_bytes()).unwrap();
        let out = Command::new(BIN).args(["parity", path.to_str().unwrap()]).output().unwrap();
        let _ = std::fs::remove_file(&path);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        // A crash (terminated by signal -> no exit code) means the host process itself died.
        assert!(
            out.status.code().is_some(),
            "witchy crashed (signal) on seed {seed} — host-level memory unsafety.\n--- program ---\n{src}\n--- stderr ---\n{stderr}"
        );
        if stdout.contains("DIVERGE") || stderr.contains("DIVERGE") {
            panic!(
                "BACKENDS DIVERGE on seed {seed} — a codegen/memory bug.\n--- program ---\n{src}\n--- output ---\n{stdout}{stderr}"
            );
        }
        if out.status.success() {
            agree += 1;
        } else {
            // Non-DIVERGE exit-1 = the generator produced a non-compiling program; tolerated.
            compile_skipped += 1;
        }
    }
    // Sanity: the generator must mostly produce compiling programs, or it isn't testing codegen.
    assert!(
        agree * 2 >= programs,
        "fuzzer mostly produced non-compiling programs ({agree} agree, {compile_skipped} skipped) — fix the generator"
    );
    eprintln!("differential fuzz: {agree} agree, {compile_skipped} compile-skipped of {programs}");
}
