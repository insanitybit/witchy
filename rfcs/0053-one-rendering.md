---
rfc: 0053
title: "One rendering system: interpolation and say through Show"
status: proposed
created: 2026-07-03
predecessors:
  - "0046 (typed trait dispatch — the bounded-impl and site-typing machinery this consumes)"
  - "0047 (equality coherence — the sibling that rejects ==(fn, fn); rendering deliberately differs, see Design)"
  - "scratch/consistency-analysis-2026-07-03.md §4 (two rendering systems with disjoint domains)"
tracking:
---

# RFC-0053: One rendering system — interpolation and `say` through `Show`

## Summary

"How does my type print?" has two answers today: `"${x}"` is always structural
and ignores a user's `Show` impl, while `say(console, x)` honors it — probed:
the same `Point` with a custom `Show` prints `Point(1, 2)` via interpolation
and `P<1,2>` via `say`; a `Duration` prints `90000` via interpolation and
`1m30s` via `say`. Meanwhile each system has holes the other doesn't:
interpolation of a `Set`, closure, capability, `Bytes`, or `Nil` **passes
`witchy check` and then fails at codegen**, and `say` rejects lists, tuples,
dicts, and Options (no blanket `impl Show for List(a) where a: Show`). This
RFC makes rendering one path: the structural renderer becomes the *derived
default `Show`*, interpolation consults a user `Show` impl when one exists,
blanket impls close `say`'s container holes, and the check-passes-codegen-
fails class is eliminated by giving every first-class value a stable rendering
— specified as a single render table both backends must satisfy.

## Motivation

All probed at HEAD against the PATH binary:

- **Two answers to one question.** `spec/language.md:67-76` teaches "reach for
  interpolation first: `"${x}"` renders *any* value" and positions `Show` as
  the custom-rendering route — but interpolation never consults the impl, so
  writing a `Show` changes `say` and `show_list` output while every `"${x}"`
  in the program keeps the structural form. The two most common print idioms
  disagree about the same value.
- **The codegen-fail class.** `witchy check` passes, then the compile fails,
  for `"${set}"`, `"${closure}"`, `"${console}"`, `"${bytes}"` — all four
  with one shared, mostly-wrong diagnostic ("typically a generic record such
  as `Set`… call `set.show(s)`", emitted at
  `crates/witchy-lower/src/codegen/builtins.rs:288-300`) — and `"${Nil}"`
  fails with the barer "reached a construct the compiled backend does not
  support". This is exactly the interpreter-only-features-at-zero class the
  project forbids. (The consistency report also listed ranges here; re-probed
  at HEAD, `"${0..5}"` renders `[0, 1, 2, 3, 4]` with parity agreeing — ranges
  are out of scope as already-working.)
- **`say`'s holes.** `say(console, [1,2,3])` → type error "`List<Int>` does
  not implement `Show`"; likewise tuples, dicts, `Some(5)`. `show.show_list`
  exists as a *named workaround* for exactly one of the missing blanket impls.
- **Duration is the visible casualty**: raw ms in interpolation, `1m30s` via
  `say` — the value most likely to be shown to a human is the one that splits.

## Design

### One path: `render(x)`

Define one function of the semantics, `render(x)`:

1. If `x`'s concrete type has a user (or std) `impl Show` → `show(x)`.
2. Otherwise → the **derived structural `Show`**: the current structural
   renderer, now specified as the default `Show` every type gets.

Interpolation (`"${x}"`), the `to_string` builtin, and `say` all mean
`render(x)`. Nothing else renders. Consequences:

- For types **without** a custom `Show`, output is byte-identical to today —
  zero migration for most programs.
- For types **with** a custom `Show`, every `"${x}"` in the program starts
  honoring it. This is an observable change and the point of the RFC; it is
  called out as breaking below.
- `spec/language.md:67-76` is rewritten: interpolation doesn't merely give "a
  structural default *like* `Show`" — it *is* `Show`, derived unless you
  write one.

**Dependency on [RFC-0046](0046-typed-trait-dispatch.md).** Step 1 needs the
concrete type of an arbitrary interpolated expression at the call site — the
exact capability the string-encoded shadow dispatch cannot deliver and 0046's
TypeTable threading does. The compiled backend already monomorphizes per-shape
renderers (`ts_helpers` / `ts_{id}`, `crates/witchy-lower/src/codegen/mod.rs:698-810`);
the change is that a shape whose type has a `Show` impl compiles its renderer
as a call to that impl instead of the structural walk. The interpreter
mirrors: its `Display for Value` walk becomes the *derived* arm, entered only
after an impl lookup misses.

### Blanket impls close `say`'s holes

With 0046's bounded-impl machinery, std/show gains:

```
impl Show for List(a) where a: Show
impl Show for Option(a) where a: Show
impl Show for Dict(k, v) where k: Show, v: Show
impl Show for Set(a) where a: Show
impl Show for (a, b) where a: Show, b: Show     # per tuple arity, as derives do
```

Each is the derived structural form over the elements' `render` — so a
`List(Point)` under a custom `Point` Show prints `[P<1,2>, P<3,4>]`
everywhere. `say` then accepts any `Show` value, which — with the derived
default — is any value. `show.show_list` and `set.show` are **deleted** in
the same cut (break-don't-deprecate): both were named workarounds for the
missing impls, and their call sites become plain `"${xs}"`/`say`.

### Duration renders humanely, everywhere

`Show for Duration` is already `duration.human` (`std/show.witchy:34-40`);
under one path, interpolation follows: `"${90000ms}"` → `1m30s`. **Breaking**
for any program that parsed interpolated raw ms — the escape hatch is explicit
(`"${duration.ms(d)}"`), and the differential corpus + book fences catch every
in-tree occurrence.

### The codegen-fail class dies: opaque forms, not type errors

**Decision: render, don't reject.** `Nil`, closures, and capabilities get
stable opaque renderings (table below) rather than becoming typecheck-time
interpolation errors. Rationale: rejecting at check time breaks
debug-printing ergonomics — `"${x}"` inside a generic or exploratory context
must never be the thing that won't compile; an opaque form is useless to
*parse* but harmless to *print*. This deliberately differs from
[RFC-0047](0047-one-equality.md), which **rejects** `==` on functions
and capabilities: equality is a semantic judgment programs branch on (a wrong
or backend-dependent answer corrupts behavior — the probed `f == f`
interp-true/compiled-false divergence), while rendering is a human-facing
projection where a stable opaque token is a correct, total answer. Rendering
and equality may coherently have different domains because only one of them
feeds back into program logic.

Either way the invariant is: **a program that passes `witchy check` compiles,
and a rendering that can't be supported fails at check time** — loud at check
or working at runtime, never a codegen surprise. The `reject_reason` channel
at builtins.rs:288-300 becomes dead for rendering and is deleted.

### The render table (acceptance spec — both backends, byte-identical)

| Type | `render` output | Notes |
|---|---|---|
| `Int` | `42` | unchanged |
| `Float` | `render_float` form | unchanged (shared `witchy_syntax::fmt`) |
| `Bool` | `true` / `false` | unchanged |
| `String` | the string, bare | unchanged; **stays unquoted inside containers** (see Alternatives) |
| `Nil` | `Nil` | interpreter already does this; compiled: new |
| `Duration` | human form `1m30s` | **changed** in interpolation (was raw ms) |
| `List(a)` | `[e1, e2]`, elements via `render` | element custom Shows now honored |
| tuple | `(e1, e2)` | " |
| record / ADT | custom `Show` if impl'd, else `Name(f1, f2)` | **changed** when an impl exists |
| `Dict(k, v)` | `{k1: v1, k2: v2}` | insertion order, as today |
| `Set(a)` | `{e1, e2}` | compiled: new (was codegen-fail); matches deleted `set.show` |
| `Bytes` | `Bytes(len=N)` | interpreter's existing form; compiled: new |
| range | `[0, 1, 2]` | already works both backends; unchanged |
| closure / fn value | `<fn>` | interpreter changes from `<function/N>`; compiled: new |
| capability | `<Console>`, `<Dir>`, `<Net>`, `<Secret>`, … | interpreter standardizes its `<capability …>`/`<dir>` zoo; compiled: new |

Every row gets a differential test; the table is the DoD. The
`WITCHY_TYPE_CHECK` sanitizer's tag machinery gives the compiled opaque rows
their type names for free where a header tag exists.

## Alternatives

- **Do nothing / document the two systems.** Leaves the spec's own §"Rendering
  values" internally false and the codegen-fail class alive. Rejected.
- **Reject `${Nil}`/`${closure}`/`${capability}` at typecheck** instead of
  opaque forms. Cleanly kills the codegen surprise and mirrors 0047's
  equality rejection — but breaks debug printing in generic code (a `where`
  fn interpolating a type-var param would need a `Show` bound today it can't
  always state) and makes `say` and interpolation diverge again at the domain
  edge. Rejected for ergonomics; revisitable if opaque forms prove to mask
  bugs in practice.
- **Make interpolation always-structural and `say` the only Show path**
  (i.e. bless today's split as design). Coherent, but it makes writing a
  `Show` impl nearly pointless (interpolation is the dominant idiom — probed
  across std/examples) and contradicts the spec's framing. Rejected.
- **Quote strings inside containers** (Rust `Debug`-style `["a", "b"]`).
  Fixes the real ambiguity `["a, b"]` vs `["a", "b"]`, but breaks the
  byte-identical-for-unshowed-types guarantee that makes this RFC cheap, and
  witchy has one renderer, not a Display/Debug pair — quoting inside but not
  outside would split `"${s}"` from `"${(s,)}"` on the same value. Rejected;
  recorded as a known, documented ambiguity of the structural form.
- **A separate `Debug`-like second trait.** Two traits is two systems with
  better names — the disease this RFC treats. Rejected.

## Drawbacks

- **Observable change for every type with a custom `Show`**: all their
  interpolations switch to the custom form at once. In-tree fallout is found
  by the suite/fences; out-of-tree programs change silently (output, not
  behavior — but golden-output tests will fail). Must headline the release
  notes with the Duration row, the likeliest to bite.
- **Hard dependency on RFC-0046.** The impl-consulting step and the blanket
  impls are both blocked on TypeTable-backed dispatch; shipping this first
  would mean interpolation consults impls only where the shadow dispatch can
  see — recreating an allowlist. This RFC must not land piecemeal.
- **Compiled-side renderer growth**: five new shape renderers (Set, Bytes,
  Nil, closures, capabilities) and impl-call indirection in `ts_` helpers —
  more monomorphized code per program, and the `ts_`/WIR-twin seam
  (`ts_helpers` + `wir ts_{id}`) gets more load-bearing before any
  [RFC-0050](0050-method-call-generalization.md) cleanup reaches it.
- **Opaque forms can hide mistakes**: printing `<fn>` where a user meant to
  *call* the function is now silent-but-visible instead of a compile error.
  The equality side stays loud (0047), which bounds the damage to output.
- Deleting `show_list`/`set.show` breaks their callers (in-tree: few; the
  fix is shorter code), and the `Show for String` identity impl means
  `render` on a bare String is unquoted by spec — the container-ambiguity
  wart is now written down rather than fixed.

## Prior art

- Rust's `Display`/`Debug` split and `impl Debug for Vec<T> where T: Debug`
  blanket impls — the bounded-impl shape adopted here, minus the two-trait
  split (deliberately: one renderer, witchy-sized).
- Python's `__str__`/`__repr__` — the same lesson from the other direction:
  two rendering protocols is a permanent teaching burden; witchy picks one.
- `std/show.witchy`'s own header — which already describes exactly this
  design's *intent* ("implement `Show` … to give a type a custom readable
  form") and documents the Duration split this RFC closes.
- RFC-0037's differential harness — the render table lands as one more
  oracle-checked corpus, which is what makes "byte-identical on both
  backends" enforceable rather than aspirational.

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** All motivation probes reproduce: the custom-Show
split; check-passes-but-codegen-fails for sets/closures; the say holes.
Materially stale in the favorable direction: blanket
`impl Show for List(a) where a: Show` ALREADY WORKS (probed, parity green) — so
the non-breaking say-holes slice is landable now, independent of the
interpolation flip.

**Required revisions.** (a) Add a rule + differential-table rows for user code
trapping / recursing / exhausting the stack inside render — divergence-prone
between the interpreter's native stack and the WASM engine's limits, i.e.
currently a fresh parity hole inside the feature meant to be byte-identical.
(b) Name the interpreter Display → fallible-evaluator-method refactor honestly.
(c) Make BUG-004 (address-keyed TypeTable) re-verification a precondition — a
stale table hit would silently swap renderings identically on both backends,
invisible to the harness. (d) Release-note `to_string`-as-data (dict keys)
beyond golden output.

**Verdict.** Needs-revision; the blanket container-Show slice is landable now.
Priority: medium. Sequencing: after RFC-0046's shadow deletion + BUG-004.
