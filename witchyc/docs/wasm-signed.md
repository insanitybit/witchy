# I64

```
let a: I64 = 7
let b = -3
console.print_i64(a / b)      // -2
point.x += dx
let bits = I64.bits(a)          // the same 64 bits as a U64
```

`I64` is a signed 64-bit number. It is a Copy scalar like `U64`: a local, a
field, an array or `Vec` element, a parameter or a result.

## What the language gives

- Literals: a number where an `I64` is expected, from -9223372036854775808 to
  9223372036854775807. One outside the range is refused at compile time.
- `-x`, `+ - * / %`, `+=` and `-=`, and the six comparisons, between two `I64`.
- `I64.from_bits(u)` and `I64.bits(i)`, which keep the 64 bits as they are.

An `I64` and an unsigned number never meet in one operation, and neither is
passed where the other is expected; there is no implicit conversion. `-x` on an
unsigned number is refused.

## What traps

Every operation gives the mathematical result or traps:

- `+`, `-`, `*` and `-x` trap when the result is outside I64;
- `/` and `%` trap on a zero divisor, and `/` traps on the least I64 divided by -1.

Division truncates toward zero and the remainder takes the dividend's sign, as
in Wasm, Rust and Go. `-9223372036854775808 % -1` is 0.

`scripts/check-i64.py` compares the emitted code with Python integers for every
pair of 24 values at the edges the checks turn on: 2304 results exact, 576
traps, in ten seconds.

## The library

`library/wasm-int.wy` is safe code on those operations: `I64.from(u)`,
`to_u64`, `magnitude`, `below_zero`, `negative`, `least`, `greatest`;
`Hash`, `Eq` and `Discard`, so an `I64` is a map key or a discarded element;
`console.print_i64` and `console.write_i64`; `String.push_i64` and
`String.parse_i64`.

`checked_add`, `checked_sub`, `checked_mul`, `checked_div`, `checked_rem` and
`checked_neg` answer `Err(1)` where the plain operation would trap on a result
outside I64 and `Err(2)` on a zero divisor. They are written with comparisons
on the plain operations, so they add nothing to the compiler.
`tests/fixtures/signed/checked.wy` runs every pair of eight edge values through
each and compares the counts and sums with Python's.

## Proofs

A scalar contract may speak of I64 values. To the proof an I64 is an integer
and an unsigned number is a natural number (`docs/wasm-scalar-proofs.md`).

```
fn abs(x: I64) -> I64:
    requires x > -9223372036854775808
    ensures result >= 0
    ensures result == x || result == -x
    proof:
        auto
    if x < 0:
        return -x
    return x
```

- Clauses name I64 parameters, locals in loop invariants, `result`, fields and
  one-field methods, with negative literals, unary minus and `+ - *`.
- A function with a contract must show that its I64 arithmetic cannot trap:
  each `+`, `-`, `*` and `-x` stays inside I64, each divisor is not zero, and
  `/` is not the least I64 by -1. Without the `requires` above, `abs` is
  refused, because `-x` traps for the least I64.
- `+ - *` are followed exactly. What `/` and `%` give is an unknown value in
  range: they round toward zero, which the proof language's division does not.
  For the same reason no clause divides an I64.
- `I64.bits` and `I64.from_bits` of anything but a literal give an unknown value.
- An I64 and an unsigned number never meet in one comparison; a clause that
  mixes them is refused when it is read.
- A function proved by the memory or record model cannot use `I64` at all yet.

`tests/fixtures/signed/proved.wy` proves fourteen functions (absolute value,
clamp, sign, bounded sums, loops with invariants over I64 and over both kinds,
a guarded division, a field updated through `&mut`, a caller that relies on a
callee's contract). Fourteen false or unsafe claims are refused in
`tests/fixtures/signed/*.refused.wy`: a sum or negation that may overflow, a
divisor that may be zero, the least I64 divided by -1, a division on the
unguarded side of `||`, a difference read as a natural number, a wrong sign, a
quotient's value, a loop that breaks its invariant, a caller that does not
show a `requires`, a value lent with `&mut`, the bits of an I64.

Before this, an I64 result was unknown to the proof and its operations carried
no obligation, so a function with a contract could still trap on signed
overflow. `tests/fixtures/signed/beside.wy` proved then and needs a bound on
`x` now.

## What is trusted

- The emitter's sequences in `src/compiler/emit.rs` (`signed`): the sign test for
  `+` and `-`, the divide-back test for `*`, the two tests before `/` and `%`.
  They are tested by the differential script, not proved.
- Six more instructions in the admitted profile: `i64.lt_s`, `gt_s`, `le_s`,
  `ge_s`, `div_s`, `rem_s`. None touches memory, so the store-confinement
  argument is unchanged. The engine and the launcher admit them; a control in
  `scripts/test-wasm-allocator.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) runs each and still refuses `shr_s` and the
  rotations.
- The checker rule that keeps signed and unsigned apart (`src/compiler/signed_tests.rs`).
- The scalar walk's reading of I64 operations (`src/compiler/scalar_vc/facts.rs`, `signed`):
  that the emitted `+ - *` give the exact integer or trap, and that `/` and `%`
  trap exactly when the stated conditions fail. The differential script tests
  the emitted code against that reading; the walk itself is tested by the
  fixtures above, not proved.

## Not yet

`I8`, `I16`, `I32`; shifts on `I64`; the value of `/` and `%` in proofs;
`I64` in functions proved by the memory or record model.
