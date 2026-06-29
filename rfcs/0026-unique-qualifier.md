---
rfc: 0026
title: unique / local unique — uniqueness as a surface type
status: implemented
created: 2026-06-28
tracking:
---

# RFC-0026: `unique` / `local unique` — uniqueness as a surface type

## Summary

Promote witchy's affine ownership from an *inferred, best-effort* property into a
*declared, checked* type qualifier: `unique T`. Where the uniqueness pass today
silently de-optimizes when it cannot prove a value is unaliased (a copy via the
self-healing `__cap` token), `unique T` is the programmer asserting uniqueness as
a contract the compiler **enforces** — turning best-effort in-place reuse (RFC-0016
rung R2) into a guarantee, and unlocking destination-passing and eager-free-at-
last-use that are unsound without it. `local unique T` is the non-returnable
variant (the direct analog of Ante's `local uniq`), for uniqueness that is valid
only within the current call.

## Motivation

The information already exists. Typeck tracks a `consumed` set (use-after-move is
already a check-time error); the uniqueness pass computes per-binding
`Unique | Shared | Dead`; `Summaries::may_alias_out` carries it across calls. But
none of it is *expressible in a signature*, so:

- **It cannot compose as a contract.** A function that needs its argument unique
  to reuse it in place must re-infer that property at every call, and silently
  copies when inference fails. The caller gets no signal.
- **The strongest optimizations stay off.** Guaranteed in-place reuse,
  destination-passing (the callee writes its result into the caller's buffer),
  and eager free at last use are all unsound on a *possibly* shared value. They
  become sound exactly when uniqueness is a checked precondition rather than a
  hope — which is the whole premise of `mode opt`
  ([performance-modes.md](performance-modes.md), bucket 3, tier 3).
- **`mode opt` errors need a vocabulary.** A mode file turns a missed uniqueness
  fact into a compile error; `unique` is how the programmer states the intent the
  error is checked against, with the `why_not` provenance the pass already
  computes.

The Ante post frames the same primitive from the other side: Ante's hard problem
is *converting* a shared (`Rc`) value to `uniq` safely. witchy's values are never
shared-mutable to begin with, so `unique` is not a risky conversion — it is just
naming the affine fact the type system already half-tracks, and `local unique` is
the same "valid only in this scope, may not be returned" restriction witchy
already enforces three different ways (`borrow_escape_check`, the region
outer-assignment rule, `move`).

## Design

### The qualifier

```witchy
fn consume(xs: unique List(Int)) -> Int          // caller must hand over a unique list
fn build() -> unique List(Int)                   // result is freshly unique, reusable
fn scan(xs: local unique List(Int)) -> Int       // unique here, but not returnable
```

- `unique T` — the value is the sole reference; the callee may reuse its buffer
  in place and the result may be returned as `unique`.
- `local unique T` — unique *within this activation only*; it may be mutated in
  place but **may not be returned or stored where it would outlive the call**
  (a check-time error: `local unique cannot escape`). This is what every
  `mut → uniq` conversion actually yields; `unique` is the special case that is
  also confined to ⊑ `Call` *and* freshly allocated here, so it is safe to return.

`unique` is a one-way coercion to `shared` (an ordinary value): passing a
`unique T` where a `T` is expected is fine and drops the guarantee; the reverse
requires proof (the value is provably `Unique` at that point per
[0024](0024-unified-facts-lattice.md)) or a copy.

### Relationship to the existing conventions

`unique`/`local unique` are *type qualifiers* (they live on the type, propagate
through generics, appear in `pub` signatures); `let`/`var`/`own` are *parameter
conventions* (they describe the calling protocol). They compose:

| | meaning |
|---|---|
| `own xs: unique List(Int)` | caller transfers a value it guarantees is unique; callee may FBIP-reuse it with no copy ever |
| `xs: local unique List(Int)` | borrowed unique for the call's duration; in-place ok, escape forbidden |
| `xs: unique List(Int)` (default conv) | observably-immutable but unique view; reuse permitted, result not threaded back |

The interprocedural backbone is unchanged: `unique` on a parameter is a
*declared* `may_alias_out[i] = false` plus a *requirement* that the argument is
`Unique` at the call — checked against [0024](0024-unified-facts-lattice.md)'s
lattice instead of inferred. So annotating a hot stdlib helper `unique` both
documents the contract and certifies every call site at once.

### What it unlocks

1. **Guaranteed in-place reuse / FBIP** (RFC-0016 R2 as a contract): `unique`
   means the `cap > len` runtime branch can be elided — the mutation is
   unconditional, because uniqueness is checked, not hoped.
2. **Destination-passing.** `fn render(out: unique String, ...)` lets the callee
   construct directly into the caller's buffer with no intermediate allocation —
   the explicit form of the optimization the uniqueness pass approximates.
3. **Eager free at last use.** A `unique` value that is `last_use` per
   [0024](0024-unified-facts-lattice.md) and confined to the frame can be freed
   immediately rather than waiting for the watermark — sound only because no
   alias can resurrect it.

### Enforcement and `mode opt`

In a normal file, violating `unique` (passing a possibly-shared value where
`unique` is required, when the compiler cannot prove it) is a **copy**, not an
error — the qualifier raises precision but the self-healing token keeps the
program correct. In a `mode opt` file it is a **hard error** with `why_not`
provenance. This mirrors how `mode opt` already promotes cliffs: same fact,
severity dial turned to error. `unique` is therefore most valuable inside
`mode opt`, where it is the type-level vocabulary the mode's guarantees are
written in.

## Alternatives

- **Keep uniqueness fully inferred.** Works for normal code (the token absorbs
  imprecision) but cannot express a contract across a `pub` boundary, cannot turn
  on the contract-requiring optimizations, and gives `mode opt` nothing to check
  against. Inference and the qualifier are complementary, not competing — infer
  by default, declare where it must be guaranteed.
- **Reuse `own` for everything.** `own` is about the *calling protocol*
  (consume-on-call); `unique` is about the *value's aliasing*. They are
  orthogonal (`own unique` is a meaningful, stronger combination). Conflating
  them loses the distinction destination-passing needs.
- **Full Rust-style lifetimes for the non-returnable case.** `local unique` gets
  90% of the value (in-place, no escape) with zero lifetime syntax. Genuine
  returnable borrows/views need lifetimes and are deferred to
  [0027](0027-packed-layouts-sroa.md)'s tier-4 follow-on, not this RFC.

## Drawbacks

- A third axis of annotation (`let`/`var`/`own` × `unique`/`frozen` × type) is
  real cognitive load. Mitigated by keeping all of it *optional in normal code*:
  you reach for `unique` only when you want the guarantee or you are in
  `mode opt`. Normal witchy is unchanged.
- `unique` in `pub` signatures is a contract that can break compatibility if
  loosened later (like any tightening of a precondition). This is inherent to it
  being a contract; the `why_not` diagnostics make violations actionable.
- Distinguishing `unique` from `local unique` at return position is a subtlety
  users will trip on; the error message must spell out the fix (return a freshly
  allocated `unique`, or accept `local unique` and don't return it).

## Prior art

- [Ante: blending borrowing and reference counting](https://verdagon.dev/blog/ante-blending-borrowing-rc)
  — `uniq` / `local uniq`; this RFC adopts the local-vs-returnable distinction
  directly, minus Ante's reachability check (witchy needs none — values aren't
  shared-mutable).
- [performance-modes.md](performance-modes.md) — lists the `unique` surface
  qualifier as tier 3; this RFC is its concrete form.
- Clean/Mercury uniqueness types; Koka/Lean 4 Perceus reuse — affine ownership as
  a checked type enabling in-place reuse.

---

> 2026-06-29: **Implemented — as a CONTRACT; the optimization it gates is already
> delivered by uniqueness inference + the RC-floor reuse rung.** `unique T` /
> `local unique T` parse as `Type::Qualified` qualifiers (shared with [0025]),
> format/round-trip, thread through signatures, and lower to the inner type
> (parity-neutral). Enforcement: a `local unique` value is valid only within the
> call, so it may not escape — a `local unique` RETURN type is a check-time error
> (`unique` is the returnable form); and unlike `frozen`, `unique`/`local unique`
> are deliberately compatible with `var` (in-place mutation/FBIP is the whole point).
>
> The optimizations `unique` was meant to UNLOCK — guaranteed in-place reuse,
> destination-passing, eager free at last use — are ALREADY realized by inference:
> the uniqueness pass (`__cap`, interprocedural `may_alias_out`) does in-place
> mutation wherever it can prove non-aliasing, and the RC-floor reuse rung (RFC-0016)
> reuses confined buffers; the `own` convention already transfers ownership for
> FBIP. `unique`'s residual marginal win is eliding the FIRST re-own copy across a
> `pub` boundary where inference is conservative — a single copy, not measured worth
> a dedicated `WITCHY_OPT` lever (which would be near-vacuous; excluded per RFC-0030's
> no-phantom rule). So `unique` ships as a checked CONTRACT / API-expressiveness
> feature (and `mode opt` error vocabulary), with its performance intent met by
> construction. Full argument-uniqueness checking at `unique`-param call sites is a
> future refinement. Marking implemented.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
