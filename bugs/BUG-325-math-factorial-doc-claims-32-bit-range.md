# BUG-325: std/math.witchy factorial doc-comment (rendered into spec/stdlib.md) claims a 32-bit Int range with a 13! overflow cliff; Int is 64-bit — 13!..20! are exact and the real silent-wrap boundary is 21!

Severity: LOW
Status: FIXED
Verified: 2026-07-08 SOURCE on master a8cc2cc
Component: std/math.witchy, generated spec/stdlib.md, docs

## Problem

Current source and generated docs now describe the 64-bit behavior correctly:
`math.factorial` is exact through 20!, and 21! overflows and wraps.

## Historical Problem

`spec/language.md:47`: Int is "64-bit signed". The generated doc
(`spec/stdlib.md:1418`, rendered from `std/math.witchy:107-108`) says "Watch the
32-bit range: factorial grows past it quickly (13! already overflows)" — wrong
width (32 vs 64 bit) and therefore a wrong overflow cliff (13 vs 21). The warning
steers users away from perfectly-exact results (13!..20!) while giving no warning
at the actual silent-wrap boundary.

`math.factorial(13)` → 6227020800 (exact); `math.factorial(20)` →
2432902008176640000 (exact, the largest n! that fits in i64);
`math.factorial(21)` → -4249290049419214848 (silent two's-complement wrap). Both
backends agree — this is purely a documentation-accuracy defect, likely a fossil
from a former 32-bit Int (12! fits i32, 13! does not). The wrap itself is
spec-conformant (Int wraps).

LOW: no wrong behavior, no parity divergence.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W parity scratch/ultra-num/t_factorial.witchy && $W scratch/ultra-num/t_factorial.witchy
✓ ... interpreter and compiled WASM agree (3 lines)
6227020800                       # factorial(13) — exact
2432902008176640000              # factorial(20) — exact, largest n! in i64
-4249290049419214848             # factorial(21) — silent wrap
```

Probe: `/Users/cobrien/workspace/witchy/scratch/ultra-num/t_factorial.witchy`.

## Code evidence

- Filing-time `std/math.witchy:113-116` — the source doc-comment ("Watch the
  32-bit range … 13! already overflows").
- Filing-time `spec/stdlib.md:1484-1486` — its generated rendering.
- Control: the same file's arithmetic behaves per the 64-bit spec — factorial(20)
  is bit-exact, only factorial(21) wraps (20! < 2^63-1 < 21!).
- Distinct from BUG-240 (abs(Int.MIN) wrong behavior) and BUG-222 (nonexistent
  APIs).

## Fix direction

Edit the `std/math.witchy:107-108` doc-comment to describe the 64-bit reality:
factorial is exact through 20!, and 21! silently wraps (Int is two's-complement
64-bit). Then regenerate `spec/stdlib.md` via `witchy doc std/*.witchy >
spec/stdlib.md` per CLAUDE.md. No code change.
