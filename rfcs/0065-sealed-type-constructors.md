---
rfc: 0065
title: Sealed type constructors — smart constructors that enforce invariants
status: proposed
created: 2026-07-05
predecessors:
  - "0002 (user-definable capabilities — introduces sealing for `capability` types; this RFC generalizes it)"
  - "bugs/BUG-238, BUG-252, BUG-256, BUG-367, BUG-224 (invariant-bypass defects a sealed constructor fixes)"
  - "bugs/BUG-313 (the sealing-bypass being fixed in parallel; sealing must be airtight for this RFC to hold)"
tracking:
---

# RFC-0065: Sealed type constructors — smart constructors that enforce invariants

## Summary

Let any `type` seal its data constructor(s), so a value of that type can only be
built by code in its defining module. External code must go through the module's
public functions ("smart constructors"), which are then the single choke point
where an invariant is established and can be trusted everywhere else. This is the
exact mechanism `capability` types already use (RFC-0002 seals them so only the
host mints them); this RFC removes the restriction that only `capability` types
may be sealed.

## Motivation

Several stdlib types carry an invariant that their *named* constructors enforce
but their *data* constructor silently bypasses, because the data constructor is
public:

- `Set` promises distinct members, but `Set([1,1,2])` keeps the duplicate
  (BUG-238) — the invariant lives only in `set.from_list`, not in the type.
- `DateTime(2026, 13, 40, …)` formats an impossible date while `time.civil(…)`
  rejects it (BUG-252): validation is in the smart constructor, not the type.
- `random.Rng(0)` / a negative seed break the PRNG contract that `random.seed`
  would have upheld (BUG-256).
- coven's envelope constructors default missing fields to `""` because the raw
  record constructor is reachable (BUG-367); an unknown record-state string
  parses as `Staged` (BUG-224) for the same reason.

In each case the fix a reviewer reaches for is "make the invalid state
unrepresentable" — but the language gives no way to say "this type may only be
built by its own module." Today that guarantee exists solely for `capability`
types. The result is a recurring class of bug (a validating constructor beside a
non-validating one) that no amount of per-site fixing removes, because the hole
is the public data constructor itself.

## Design

A type may be declared `sealed`:

```
sealed type Set(a):
    SetData(List(a))          # data constructor — private to this module

pub fn from_list(xs: List(a)) -> Set(a):
    SetData(dedup(xs))        # the ONE place the invariant is established
```

- **Construction is home-module-only.** `SetData(…)` and the record/enum
  literal form resolve only inside the module that declares the type. Any other
  module (qualified `set.SetData(…)` included — this is precisely BUG-313) is a
  compile-time error: "`Set` is sealed; construct it through its module's
  functions." *Reading* fields and *matching* are unaffected — sealing restricts
  building, not inspection (the same rule capabilities already follow).
- **Smart constructors are ordinary `pub fn`s** in the module. They are the only
  way out-of-module code obtains the value, so any invariant they enforce
  (dedup, range check, field validation) holds for every value that exists.
- **One mechanism, not a new one.** Enforcement reuses the existing sealing pass
  (`seal_block` in `crates/witchy-syntax/src/linker.rs`, driven by the `sealed`
  flag on a type). RFC-0002 already sets that flag for `capability`; this RFC
  lets the `sealed` keyword set it for any `type`. No new analysis, no
  per-type special-casing — the general convention the codebase already runs.
- **Zero runtime cost, backend-parity by construction.** The check is entirely
  at link/type-check time; neither backend emits anything for it, so the
  interpreter and compiled WASM agree trivially (a sealed misuse fails to
  compile on both). This is why the fix belongs here and not in a runtime guard.

## Definition of done

1. `sealed type …` parses and sets the same `sealed` flag `capability` sets;
   `witchy fmt` round-trips it.
2. The linker rejects out-of-module construction of a sealed type — including
   the module-qualified form (BUG-313) — with a clear diagnostic, on both
   backends, and a shape/behaviour test pins it.
3. The four invariant-bearing types are sealed behind smart constructors:
   `Set` (BUG-238), `time.DateTime` (BUG-252), `random.Rng` (BUG-256), and the
   coven envelope/record-state types (BUG-367, BUG-224). Each closes its bug
   with a regression test that the raw constructor is now unreachable.
4. Sealing does not restrict matching/field-read; existing programs that only
   *inspect* these types keep compiling (a sealed-read test guards this).

## Alternatives

- **Runtime validation in every constructor**: doesn't make the invalid state
  unrepresentable — a second non-validating path can always reappear (the exact
  history of these bugs), and it costs a check on every construction. Sealing
  moves the guarantee into the type, checked once at compile time.
- **A `private` marker on individual constructors** (rather than on the type):
  finer-grained but a larger surface; the type-level `sealed` matches the
  existing `capability` mechanism and covers every current need. Constructor-
  level privacy can be a later refinement if a type genuinely needs a mix.
- **Do nothing**: the validating/non-validating constructor pair persists across
  the stdlib, and each new invariant-bearing type re-introduces the hole.

## Drawbacks

- A sealed type's module must expose enough smart constructors for legitimate
  uses; too few is a usability wall (mitigated: these types already have the
  smart constructors — `from_list`, `civil`, `seed` — the sealing just makes
  them the *only* door).
- One more keyword. It reads as the dual of `pub`: `pub` widens access, `sealed`
  narrows construction. The pairing is learnable and matches `capability`, which
  users already meet early.

## Prior art

- RFC-0002 (`capability` sealing — the identical enforcement, here generalized),
  RFC-0038 (`grantable` sealed capabilities). BUG-313 (the qualified-constructor
  bypass) must be fixed for sealing to be airtight; this RFC assumes that fix.
- "Make illegal states unrepresentable" / smart constructors (ML, Haskell's
  module-abstract types, Rust's private fields + `pub fn new`). witchy already
  has the enforcement engine; this is the surface that lets stdlib and user code
  use it.
