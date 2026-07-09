# BUG-141: RFC-0049 `random` -> `prng` rename is not reflected in stdlib

- **Severity:** LOW
- **Status:** FIXED
- **Verified:** 2026-07-08 SOURCE on master 0352504 (`std/prng.witchy` ships, the linker registers `prng`, no `std/random.witchy` is bundled, and the dice example imports `prng`)
- **Component:** `std/random.witchy`, std module registry, examples/docs, RFC-0049 naming consistency
- **Found:** 2026-07-05

## Summary

Current source has completed the RFC-0049 rename. The deterministic PRNG module
ships as `std/prng.witchy`, `crates/witchy-syntax/src/linker.rs` registers
`prng`, `std/rand.witchy` remains the capability-backed randomness surface, and
the dice example/docs import `prng`.

## Historical Problem

RFC-0049 says the security-relevant naming decision was adopted: rename the pure
seeded `random` module to `prng`, while keeping `rand` for capability-backed
CSPRNG draws. At filing time, the tree still shipped `std/random.witchy`, had no
`std/prng.witchy`, registered `random` in the bundled std module list, and kept
the dice example/docs on `import random`.

That leaves the exact confusion RFC-0049 called out: `random` is the pure
Park-Miller LCG, while `rand` is the host-capability randomness surface.

## Evidence

- `rfcs/0049-naming-lexicon.md:115-131` says the adopted recommendation is
  `random -> prng`, because confusing pure `random` with capability-backed
  `rand` is security-relevant.
- `rfcs/0049-naming-lexicon.md:217-219` later confirms the status spot-check:
  `random = LCG with exactly 1 importer`.
- The filing-time tree contained `std/random.witchy` and no `std/prng.witchy`.
- `std/random.witchy:1-6` still describes the pure deterministic Park-Miller
  generator under the `random` module name.
- `crates/witchy-syntax/src/linker.rs:98-103` registers bundled std modules and
  includes both `random` and `rand`, but no `prng`.
- `crates/witchy-syntax/src/linker.rs:290-291` maps `random` to
  `std/random.witchy` and `rand` to `std/rand.witchy`.
- `book/src/appendix-stdlib.md:49-53` lists `random` as the seeded
  pseudo-random module.
- `spec/stdlib.md:1724-1746` renders the stdlib reference section as
  `## random`.
- `examples/dice/src/dice.witchy:1-16` and `examples/dice/README.md:3-8`
  still teach `import random`.

## Why this is a release gap

The project has both a pure deterministic PRNG and a capability-backed CSPRNG.
The names need to keep that boundary sharp. A release that says the naming RFC
is adopted while the shipped stdlib still exposes the rejected name feels
half-migrated and can send users toward the wrong randomness primitive.

This is distinct from BUG-140, which tracks the capability references omitting
the `Rand` root capability. This bug tracks the pure module's public name and
RFC-0049 completion.

## Expected

Pick one state and make the repo coherent:

- Complete RFC-0049's adopted rename: `std/random.witchy` -> `std/prng.witchy`,
  bundled module registry `random` -> `prng`, dice example/docs import `prng`,
  regenerated stdlib reference, and any compatibility/deprecation note if
  desired; or
- Reopen/revise RFC-0049 to say the rename was not adopted and document why the
  pure module remains `random`.

## Acceptance

- `rg "\\brandom\\b" std examples book spec crates/witchy-syntax/src/linker.rs`
  no longer finds current public API/docs for the pure PRNG module, except in
  migration notes.
- `std/prng.witchy` exists and is registered in the bundled std module list, or
  RFC-0049 no longer claims the rename was adopted.
- The dice example and stdlib appendix/reference use the same final module name.
