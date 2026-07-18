---
rfc: 0053
title: "One rendering system: interpolation and say through Show"
status: implemented
created: 2026-07-03
predecessors:
  - "0046 (typed trait dispatch — the bounded-impl and site-typing machinery this consumes)"
  - "0047 (equality coherence — the sibling that rejects ==(fn, fn); rendering deliberately differs, see Design)"
  - "scratch/consistency-analysis-2026-07-03.md §4 (two rendering systems with disjoint domains)"
tracking: "IMPLEMENTED — blanket-Show + typed interpolation flip both shipped"
implementation-notes: |
  AS BUILT (2026-07-10, coherence reconciliation):
  - Interpolation still desugars at lex time to an internal render intrinsic,
    now spelled `@render(x)` in the compiler AST/token stream so user source
    cannot call the generated seam directly. The lexer remains type-free.
  - The semantic flip lives in [`crates/witchy-types/src/traits.rs`](../crates/witchy-types/src/traits.rs), inside
    `Mono::walk_expr`, after RFC-0046's TypeTable can report the concrete type of
    `x`. That is the only place generated render calls are rewritten.
  - Source-spellable `__render(x)` is rejected as compiler-private; the corpus
    uses interpolation or `show.render(x)`, and only generated `@render(x)` can
    reach the rewrite.
  - `std/show.witchy` exposes `pub fn render(x: impl Show) -> String: show(x)`.
    `show` is a prelude module. When the concrete type has a relevant `Show` path,
    monomorphization rewrites generated render calls to `show.render(x)`. That then
    specializes through the same bounded-generic machinery as any other `Show`
    call, so interpreter and compiled backend parity follows from one AST rewrite.
  - Imports never select rendering semantics. `show` is linked for every program;
    `import show` is accepted but redundant. `"${90000ms}"`, `show.render(90000ms)`,
    and `show.say(console, 90000ms)` all use the human Duration form.
  - The rewrite predicate keeps primitives structural, flips `Duration`, flips any
    named type carrying a `Show` impl, recurses through `List`, `Option`, `Result`,
    `Dict`, and tuples, and always flips `Set`
    because `Set([1, 2])` structurally and `{1, 2}` through `Show` are different
    public renderings.
  - The previous early `custom_show`/`Ctx::render_flip` approach was removed: it
    could not see generic container specializations such as `Set(Int)`, which left
    interpolation and `show.say` disagreeing.

---

# RFC-0053: One rendering system — interpolation and `say` through `Show`

## Summary

Before this RFC, "How does my type print?" had two answers: `"${x}"` was always
structural and ignored a user's `Show` impl, while `say(console, x)` honored it.
The same `Point` with a custom `Show` printed `Point(1, 2)` via interpolation
and `P<1,2>` via `say`; a `Duration` printed `90000` via interpolation and
`1m30s` via `say`. Each system also had holes the other did not:
interpolation of a `Set`, closure, capability, `Bytes`, or `Nil` **passed
`witchy check` and then failed at codegen**, and `say` rejected lists, tuples,
dicts, and Options (there was no blanket `impl Show for List(a) where a: Show`). This
RFC makes rendering one path: the structural renderer becomes the *derived
default `Show`*, interpolation consults a user `Show` impl when one exists,
blanket impls close `say`'s container holes, and the check-passes-codegen-
fails class is eliminated by giving every first-class value a stable rendering
— specified as a single render table both backends must satisfy.

## Motivation

Baseline probes recorded before implementation:

- **Two answers to one question.** [`spec/language.md:67-76`](../spec/language.md) teaches "reach for
  interpolation first: `"${x}"` renders *any* value" and positions `Show` as
  the custom-rendering route — but interpolation never consults the impl, so
  writing a `Show` changes `say` and `show_list` output while every `"${x}"`
  in the program keeps the structural form. The two most common print idioms
  disagree about the same value.
- **The codegen-fail class.** `witchy check` passes, then the compile fails,
  for `"${set}"`, `"${closure}"`, `"${console}"`, `"${bytes}"` — all four
  with one shared, mostly-wrong diagnostic ("typically a generic record such
  as `Set`… call `set.show(s)`", emitted at
  [`crates/witchy-lower/src/codegen/builtins.rs:288-300`](../crates/witchy-lower/src/codegen/builtins.rs)) — and `"${Nil}"`
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

### One path: `show.render(x)`

`show` is a prelude module, so imports cannot change rendering behavior:

1. If `x`'s concrete type has a relevant `Show` path, interpolation rewrites
   to `show.render(x)`.
2. Otherwise interpolation keeps the internal render call, the structural default.

`show.say(console, x)`, `show.render(x)`, and interpolation share the same
protocol. Consequences:

- For types without a `Show` path, structural output remains byte-identical.
- For types with a `Show` path, `"${x}"`, `show.render(x)`, and
  `show.say(console, x)` agree.
- `Set(a)` uses `Show` because its structural
  fallback (`Set([1, 2])`) and public display form (`{1, 2}`) differ even when
  `a` is primitive.

**Dependency on [RFC-0046](0046-typed-trait-dispatch.md).** Step 1 needs the
concrete type of an arbitrary interpolated expression at the call site — the
exact capability the string-encoded shadow dispatch cannot deliver and 0046's
TypeTable threading does. The shipped hook uses that typed table during
monomorphization to rewrite selected generated render calls to `show.render(x)`;
both backends then run the same rewritten program.

### Blanket impls close `say`'s holes

With 0046's bounded-impl machinery, std/show gains:

```
impl Show for List(a) where a: Show
impl Show for Option(a) where a: Show
impl Show for Dict(k, v) where k: Show, v: Show
impl Show for Set(a) where a: Show
impl Show for (a, b) where a: Show, b: Show     # per tuple arity, as derives do
```

Each is the structural container form over the elements' `Show` rendering — so
a `List(Point)` under a custom `Point` Show prints `[P<1,2>, P<3,4>]`
everywhere. `show.show_list` and `set.show` are retired workaround names;
callers use interpolation, `show.render`, or `show.say`.

### Duration renders humanely

`Show for Duration` is `duration.human` (`std/show.witchy`), so
`"${90000ms}"` always renders as `1m30s`.

### Unsupported structural rendering is explicit

The current compiler does not promise total opaque rendering for every value.
Function interpolation is rejected at check time, and shapes the compiled
structural renderer cannot build now get a diagnostic pointing users at
`show.render`/`show.say` when the value has a `Show` path.

The invariant for 0.1 is narrower but enforceable: interpolation either follows
the `Show` protocol, uses a structural form both backends support, or
fails loudly before runtime with a diagnostic that names the public rendering
protocol. Silent `check`-passes/codegen-surprise paths are bugs.

### The render table (acceptance spec — both backends, byte-identical)

| Type | `render` output | Notes |
|---|---|---|
| `Int` | `42` | unchanged |
| `Float` | `render_float` form | unchanged (shared `witchy_syntax::fmt`) |
| `Bool` | `true` / `false` | unchanged |
| `String` | the string, bare | unchanged; **stays unquoted inside containers** (see Alternatives) |
| `Nil` | `Nil` | interpreter already does this; compiled: new |
| `Duration` | human form `1m30s` | through `Show` |
| `List(a)` | `[e1, e2]`, elements via `render` | element custom Shows now honored |
| tuple | `(e1, e2)` | " |
| record / ADT | custom `Show` if impl'd, else `Name(f1, f2)` | **changed** when an impl exists |
| `Dict(k, v)` | `{k1: v1, k2: v2}` | insertion order, as today |
| `Set(a)` | `{e1, e2}` | generic-container follow-up shipped in `Mono::walk_expr` |
| `Bytes` | structural/backend-supported form | total opaque rendering deferred |
| range | `[0, 1, 2]` | already works both backends; unchanged |
| closure / fn value | rejected for interpolation | check-time error |
| capability | structural/backend-supported form or check/codegen diagnostic | total opaque rendering deferred |

Rows that are part of the shipped 0.1 contract get differential tests. The
deferred opaque rows are not release claims until their check/runtime behavior is
specified and tested.

## Alternatives

- **Do nothing / document the two systems.** Leaves the spec's own §"Rendering
  values" internally false and the codegen-fail class alive. Rejected.
- **Total opaque forms for `${Nil}`/`${closure}`/`${capability}`.** This keeps
  debug-printing ergonomic, but it was not the shipped 0.1 cut. Function
  interpolation is currently rejected at check time, matching the language's
  preference for loud unsupported semantics over backend-dependent surprises.
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
  by the suite/fences; out-of-tree rendered output changes silently (program
  control flow does not, but golden-output tests will fail). Must headline the
  release notes with the Duration row, the likeliest to bite.
- **Standard-library module names are now reserved.** A local `result.witchy`,
  `regex.witchy`, or any other file whose stem matches a bundled module is a
  compile-time error; allowing it to replace `show` or one of `show`'s
  dependencies would make rendering semantics depend on filesystem layout.
  Rename the module and its manifest/import references (the in-tree examples
  became `result_demo` and `regex_demo`).
- **Hard dependency on RFC-0046.** The impl-consulting step and the blanket
  impls are both blocked on TypeTable-backed dispatch; shipping this first
  would mean interpolation consults impls only where the shadow dispatch can
  see — recreating an allowlist. This RFC must not land piecemeal.
- **More monomorphization pressure**: routing interpolation through
  `show.render` creates the same bounded-generic specializations that `say`
  does. This is simpler semantically, but it makes the mono pass more
  load-bearing.
- Retiring `show_list`/`set.show`-style workaround names breaks their callers
  (in-tree: few; the fix is shorter code), and the `Show for String` identity impl means
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
