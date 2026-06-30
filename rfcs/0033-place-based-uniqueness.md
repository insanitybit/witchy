# RFC-0033 — Place-based uniqueness: thread in-place optimization through user types

- Status: proposed
- Motivating principle: *optimizations must thread, that's how they compound.*

## Motivation

witchy's copy-elision / linear-update optimization (RFC-0016 reuse, the `*_cap`
in-place arms, and the own-ABI cross-function threading) currently applies only
to the three builtin collection types — `List`, `Dict`, `String`. A user
abstraction over a collection does **not** get the same treatment:

```witchy
type Stack = { items: List(Int) }

fun push(own s: Stack, x: Int) -> Stack:
    s = { ...s, items: list.push(s.items, x) }
    s
```

`xs = list.push(xs, x)` in a loop is O(1) amortized (in-place). The equivalent
`s = push(s, x)` is **O(n) per call** — every update reallocates the `Stack`
record (and may copy the inner list). The optimization stops at the user-type
boundary, so it does not compound. This RFC threads it through.

## Current mechanism (where it stops)

- **Uniqueness analysis** (`analysis.rs`) tracks accumulator *variables* and a
  compile-time ownership token (`__cap`). Sound by default: a value is copied
  unless uniqueness is proven, and one copy re-establishes ownership.
- **`self_inplace_op`** — name-matched in-place arms for the 6 builtin leaf ops
  (`list.push/set_at/update_at`, `dict.insert/update`, string concat).
- **`self_own_call` / own-ABI** — threads the token across a user *function*
  boundary, but eligibility is gated to `List|Dict|String` params
  (`analysis.rs:323`).
- **Records** — `Expr::RecordUpdate` lowers in place **only when the record is
  SROA-active** (`codegen/mod.rs:2344`): a frame-confined record keeps its fields
  in `${name}$<i>` locals, so an update just rewrites a local. A record that
  *escapes* (returned out, stored in a collection, or received as a function
  parameter) is a heap object, and `RecordUpdate` reallocates it.

So two things are missing, and they are why ungating line 323 alone is inert:

1. There is no **heap-record in-place update**: `s = {...s, f: v}` on a unique
   heap record reallocates instead of reusing `s`'s slots.
2. Uniqueness is not a property of **places** (`s.items`), only of variables, so
   an inner collection op on a field can't be recognized as in-place, and a
   record param is never marked an accumulator → its returned cap is always 0.

## Design: uniqueness as a property of *places*

Generalize the unit of uniqueness from a *variable* to a **place** — a variable
or a field path (`s`, `s.items`, `s.a.b`). The invariant:

> If a place `p` is uniquely owned and unaliased, every sub-place of `p` is also
> uniquely owned. A unique place's backing storage may be mutated / reused in
> place; a non-unique place must be copied. Default is non-unique (sound).

Three coordinated capabilities fall out, each a direct analog of an existing
builtin mechanism:

- **R1 — heap-record in-place update.** `s = {...s, f: v}` (and the `s.f = v`
  sugar) on a unique-owned heap record writes `v` into `s`'s existing heap slot
  and reuses the allocation — the fixed-shape analog of `list_set_cap`. The cap
  for a record is a 0/1 ownership flag (no capacity/grow notion: records are
  fixed-shape, which makes this *simpler* than lists). This extends the existing
  SROA in-place path to the heap case.
- **R2 — field-path threading.** When `s` is a unique accumulator, `s.items` is
  unique, so `s.items = list.push(s.items, x)` lowers to the existing list
  in-place arm. This is `self_inplace_op` applied to a place whose root is a
  unique record, instead of only to a bare variable.
- **R3 — own-ABI for any heap type.** Ungate `analysis.rs:323` from
  `List|Dict|String` to any heap-allocated type (records, enums). With R1/R2 the
  body of an `own`-param function performs in-place updates, so the returned cap
  is *live*, and `s = push(s, x)` at the call site composes via the existing
  `self_own_call` (which is already name-agnostic / summary-driven).

The leaf in-place *operations* still differ per type (store a field vs append an
element) — that's inherent — but the **threading** (the cap, the analysis, the
own-ABI) becomes uniform across builtin and user types. No new per-name special
casing: R3 deletes a type allowlist rather than adding one.

## Incremental plan (each lands green + parity)

1. **R1 — heap-record in-place update.** ✅ **SHIPPED** (commit f4a0415).
   `s = {...s, f: v}` (the `s.field = v` desugaring) on a unique escaping heap
   record stores the changed field into `s`'s slots in place and keeps the
   pointer; the cold/un-owned path is the existing `mk{n}` realloc (re-owns).
   Records are fixed-shape so the token is a 0/1 owned flag — no new runtime
   helper. `analysis::InPlaceOp::RecordUpdate` is the recognizer; SROA still
   handles confined records (checked first). Proven firing
   (`stats::record_field_update_is_in_place`: forced-copy allocates 4×+ the heap)
   + parity, with the aliased `{...s, parent: s}` / `r.a = 99` cases in the
   differential sweep (interp pins value semantics; an unsound in-place would
   diverge). This closes the dominant case of the edge — `.field =` loops on
   escaping records are now O(1)/update instead of O(n) reallocs.
2. **R2 — field-path threading.** ⏳ NOT YET. `s.items = list.push(s.items, x)`
   in place needs a PERSISTENT field-cap (`{name}${field}__cap`, function-scoped
   so it survives loop iterations) threaded through the inner `*_cap` helper, plus
   the analysis recognizing `(var, field)` field-accumulators and resetting the
   field-cap when the record var is reassigned. Real codegen + analysis work, not
   a small patch.
3. **R3 — own-ABI generalization.** ⏳ NOT YET — and it's more than ungating
   `analysis.rs:323`. Ungating to user heap types (tried: compute the heap-type
   set from `module.items` record/enum names + builtins) builds, but the codegen
   own-ABI **call ABI** then bails: a PLAIN call to an own-ABI function (e.g.
   `identity(Counter(7, 0))`, not a `c = f(c)` self-call) doesn't supply the
   trailing cap arg the callee's signature now expects, so lowering rejects it.
   Closing R3 means teaching the GENERAL call-lowering path to append the cap arg
   (0 for non-threaded callers) and consume the extra cap result for every
   own-ABI callee — a change to the core call path, high-blast-radius. Defer to a
   focused pass with the differential oracle (output invariant under every
   `WITCHY_OPT` setting on both backends) as the gate; do NOT rush it.

## Parity & soundness

- The interpreter has value semantics; in-place mutation is **unobservable** when
  uniqueness genuinely holds, so parity is preserved as long as the *aliased*
  case still copies. Every increment ships a differential test (interp ==
  compiled) and defaults to copy when uniqueness is not proven — the same
  self-healing `__cap` discipline the existing arms use.
- This is the compiler's most delicate subsystem: an over-claimed uniqueness is a
  silent miscompile. Each increment must be conservative-by-default and earn
  in-place only from the proven token.

## Open questions (resolve during implementation)

- Does the `s.f = v` sugar already desugar to `RecordUpdate`, or have its own
  lowering? (Unify them onto the R1 path.)
- Enums/variants with heap payloads: same treatment as records, or defer to a
  later increment?
- Interaction with RFC-0027 SROA (confined records already bypass the heap — R1
  is specifically for the *escaping* / own-threaded case; the two must not fight
  over the same binding).

## Non-goals

- A runtime per-object refcount ("RC floor"). This RFC is the *compile-time*,
  zero-runtime-cost generalization (place-based static uniqueness). An RC floor
  is a separate, heavier tool justified only by genuinely dynamic sharing that
  static analysis cannot prove — not the case here.
