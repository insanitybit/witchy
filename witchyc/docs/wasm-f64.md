# F64

```
import float
let r = F64.sqrt(dx * dx + dy * dy)
body.vx -= dx * mass * r
console.print_f64(energy, 9)          // -0.169075164
let n = F64.to_i64(F64.floor(x))      // traps on NaN or outside I64
```

`F64` is an IEEE 754 binary64 number, a Copy scalar like `U64` and `I64`: a local, a field,
an array or `Vec` element, a parameter or a result, a closure's capture, argument or result,
and a generator's local or yielded value (`f64/closure.wy`, `f64/generator.wy`).

## What the language gives

- Literals with a point: `1.5`, `0.01`, `4.84143144246472090e+00`, the nearest F64 (as Rust
  parses them). `1` is an integer, never an F64; `-x` negates.
- `+ - * /`, `+=` and `-=`, and the six comparisons, between two F64. No `%` and no bit
  operations. Results round to nearest; division by zero gives an infinity or NaN; NaN is not
  equal to anything, itself included.
- Conversions are written out, as for I64, since an F64 and an integer never meet in one
  operation: `F64.from_u64(n)`, `F64.from_i64(i)`, `F64.to_i64(x)` (truncates toward zero;
  traps on NaN and outside I64, the only F64 operation that traps), `F64.from_bits(u)` and
  `F64.bits(x)`.
- `F64.sqrt(x)`, `F64.abs(x)`, `F64.floor(x)`.
- `sort` orders a `Vec<F64>` by IEEE totalOrder (`f64/sort.wy`, `f64/sort-total-order.wy`):
  `<` on numbers, -0.0 before 0.0, every NaN last (it was `<`, and NaNs went anywhere).
  `total_key(x)` is a U64 in the same order.

## Determinism

A NaN's payload is the one thing about an F64 result the engine may choose differently on
different machines. It shows only through `F64.bits`, which gives every NaN the canonical
NaN's bits (`f64/nan.wy`), so a run computes the same output everywhere without the engine
canonicalizing every operation (that cost 5x on n-body).

## On the console (`import float`)

`console.write_f64(x, decimals)` and `print_f64` write `x` rounded to `decimals` places (at
most 9). The digits are exact: an F64 is `m * 2^e` for integers `m` and `e`, and the library
computes `m * 10^decimals * 2^e` in 32-bit limbs and rounds half to even, as C's `printf` and
Python's format do, so a large value is written out in full and a negative zero as `-0.00`
(`f64/print-exact.wy`: 400 values from every range against Python, `f64/print.wy`). `NaN`,
`inf` and `-inf` are written as such.

## Proofs

No proof reads floating point: a contract does not name an F64. A function with a contract
may use F64 inside: the scalar proof reads an F64 result, and a comparison of two, as an
unknown value, so the contract is shown from the integers alone (`f64/proved.wy`), a false
claim is still refused (`proved-false.refused.wy`), and a true claim that needs a float's
value is not shown (`proved-float-claim.refused.wy`). Such a function does not call
`F64.to_i64`, the one F64 operation that traps, since its proof could never show the
conversion succeeds (`to-i64-contract.refused.wy`). A function proved by the memory or record
model does not use F64 (`modelled.refused.wy`), as for I64.

## Speed

`f64/nbody.wy` is the Benchmarks Game n-body, which prints the reference energies for 1000
steps. 1,000,000 steps take 0.05 s launch to exit on an M4, Go's matched program 0.03 s.
`f64/spectral.wy` is spectral-norm, which gives the reference 1.274219991 for n = 100 with
`Vec<F64>`. `f64/mandelbrot.wy` (200 by 200) matches a Python model of the same
algorithm byte for byte; 2000 by 2000 takes 0.14 s.

## The host

The engine's admission profile allows the `f64` value type and exactly the instructions the
compiler emits for these operations; everything else stays outside the profile. NaN
determinism is the compiler's: the host admits `i64.reinterpret_f64`, so a module written by
hand could see a NaN's payload, which may differ between machines.
