# Language evolution: typed lowering, value equality, a real stdlib, self, comptime

Five workstreams, one dependency spine. The learning log (`scratch/LEARNING-LOG.md`)
is the evidence base: an LLM learned witchy from the book alone and its three
serious findings (F15 silent `dict.get` divergence, F11 `${}` codegen failures,
F5 unguessable builtins) all trace to the same root — **the compiler's type
knowledge degrades after typeck**. Typeck infers every type in the program,
then throws that away; monomorphization re-guesses types from a head-name-only
scope, and codegen re-guesses them again through a patchwork of valtype maps.
Everything below either fixes that or builds on the fix.

Decisions in this document are made, not open; phases are ordered by
dependency, not preference.

## Phase 0 — the keystone: typed lowering

Thread typeck's RESOLVED types into lowering and codegen instead of
re-deriving them. Concretely: after `typeck::check` solves the module, persist
a side table of resolved types (function instantiations at each call site;
the concrete type of every binding, pattern binder, and expression that has
one) keyed the way the uniqueness pass keys its facts — by AST identity, with
a consumption check. `traits::lower*` and `codegen` consume the table instead
of `Scope`-by-head-name and the `local_*_valtype` patchwork.

This is the largest single change and it pays for every phase after it:

- **F11 closes structurally**: `to_string`/`${…}` always knows the value's
  type — ADT String payloads, `iter.collect`/`fold` returns, everything. The
  book's "interpolation renders any value on both backends" claim becomes
  true instead of aspirational.
- **F16 closes**: constructor availability stops depending on which imports
  happened to register names; patterns resolve against typeck's knowledge.
- **Monomorphization stops missing**: `dict.get(d, "host")` specializes even
  though `v` never appears at the call site, because `d`'s full
  `Dict(String, String)` type is known — the precondition for Phase 2's
  stdlib and Phase 1's equality story.
- The codegen valtype maps (`local_list_elem_valtype`,
  `fn_ret_result_valtype`, `list_nesting`, …) shrink to one lookup. The
  |Int|>2³¹-at-depth-3 residual class disappears with them.

Exit: the F11/F16 probes in `scratch/` pass parity; the big-Int
nested-collections family is closed at every depth; no behavior change
anywhere else (full differential suite + forced-copy mode green).

## Phase 1 — value equality, always

**Decision: reference equality does not exist in witchy semantics.** Pointer
comparison may exist only as an invisible fast path (`ptr == ptr` implies
value-equal — sound because values are immutable data; pointer-UNEQUAL always
falls through to structural comparison).

- Kill the silent fallback: `==`/`!=` on operands whose type is unknown at
  emission is a **loud compile error** today (ship this immediately, before
  Phase 0 — it converts F15 from silent-wrong to loud). With Phase 0, the
  case mostly stops arising; where genuine polymorphism remains, the operand
  type comes from the table or the function carries an `Eq` bound and `==`
  dispatches through the trait.
- Add the pointer-equal fast path to the structural-equality helpers
  (`$eq_*`): one `i32.eq` short-circuit at the top. Invisible, explicit,
  measured.
- Fix the stdlib casualties found by the log: `dict.get`, `has_key`,
  `merge`, `invert`, `from_pairs` (and any `list`/`set` cousins) — after
  Phase 0 their `k == key` compiles correctly; until then they get `Eq`
  bounds or builtin backing.
- Tests: a runtime-built-key parity suite (trim/split/concat/JSON-sourced
  keys), because literal-key tests pass vacuously through interning.

## Phase 2 — builtins become a stdlib (traits and functions)

**Decision: the global namespace shrinks to (a) capability operations
(`print`, `read`, `write`, `send`, `spawn`, `now`, …— authority should be
loud and unprefixed), (b) literal/ABI support (`to_string`, the interpolation
desugar target), and (c) nothing else.** Every pure data operation moves to
its module: `list.push`, `list.at`, `list.length`, `string.split`,
`string.trim`, `dict.insert`, `dict.get_or`, `dict.pairs`, … implemented as
module-qualified natives (the `src/native.rs` registry pattern) or thin
witchy wrappers over them. Indexing sugar `xs[i]` and `for … in` keep
working (they desugar to the module functions).

- Traits grow to carry the polymorphic surface: `Eq`, `Ord`, `Show` exist;
  add `Hash` (dict keys), `Len`?—no: length stays per-module (`list.length`,
  `string.length`); do add `Iter` as the protocol `for` and `std/iter`
  already share informally.
- **Migration mechanics: BREAK.** witchy is pre-prod — the builtins are
  removed outright in one change, no alias release, no deprecation notes.
  A one-shot `witchy fmt` canonicalization rewrites bare builtins to
  module-qualified calls, and the whole repository (std, examples,
  projects, book) migrates in the same commit; anyone outside the repo
  runs `fmt` once and is current. The error for a removed builtin names
  the module that now owns it ("`split` moved to `string.split`").
- The F5 finding dissolves rather than getting documented: there is no
  builtins-vs-module distinction left to explain. The book's stdlib appendix
  becomes the single reference; capability ops get their own short page.
- Ergonomics riders from the log, folded in here because they're stdlib
  surface: tuple patterns in `for` (`for (k, v) in dict.pairs(d)` — pure
  parser sugar over the existing LetTuple lowering, F4), `json.JsonFloat`
  (F12), a `string.lines` that documents (or drops) the trailing-empty
  behavior (F10), `witchy new`/`build` path handling (F17/F18).

## Phase 3 — explicit `self`, Rust-shaped methods

`self` with conventions already exists (`fn drain(own self)` works today).
This phase makes it the *rule* and finishes the half-built parts:

- **Decision: every instance method declares `self` explicitly**, with the
  full convention set: bare `self` = owned (like any parameter), `let self`
  = borrow (no-escape, typeck-enforced), `inout self` = mutate-and-write-back
  (`var c = …; c.bump()`), `own self` = consume. The uniqueness pass extends
  naturally: `inout self`/`own self` receivers join the own-ABI so builder
  chains (`c = c.with_x(1).with_y(2)`) pipeline in place.
- **Self-less functions in an `impl` are static**: callable as
  `Type.name(args)` (today that syntax mis-desugars into a method call with
  the type as receiver — fix the desugar), never as a method on a value.
- **Decision: method-call syntax narrows to real methods, immediately.**
  `x.f(a)` resolves to inherent impls, then trait impls, for `x`'s type —
  it stops being sugar for *any* free function, in one breaking change.
  (Free functions are called as functions; the module migration in Phase 2
  makes `list.push(xs, e)` the spelling, with `xs.push(e)` available
  exactly when `push` is a method.) The error at an old UFCS site names
  the function spelling to use.
- This is what "disambiguates calling conventions": the receiver's
  convention is declared on `self` in one place, visible at every call site,
  instead of inferred from how a free function happens to take its first
  parameter.

## Phase 4 — derive: the comptime seed

**Decision: comptime enters witchy as *additive item generation*, never code
modification** — generated code is appended to the module before
typeck/footprint analysis, so every existing invariant (capability checking,
the footprint computed from source, parity) applies to the *expanded*
program automatically. Nothing can be rewritten or deleted, so no macro can
launder authority out of a signature.

Start with the compiler-built derives, because Phase 2's trait-rich stdlib
makes hand-written `Eq`/`Ord`/`Show` impls the dominant boilerplate:

```witchy
type Point derive(Show, Eq, Ord):
    x: Int
    y: Int
```

expands (deterministically, in the compiler, no user code execution) to the
obvious impls. Exit: the derives produce byte-identical behavior to
hand-written impls on both backends, and `witchy doc` shows the derived
impls.

## Phase 5 — user comptime, in the sandbox witchy already has

The full feature, built on machinery that already exists: **a `comptime`
block is witchy code executed at compile time in the zero-ambient WASM
sandbox** (the same hard isolation deterministic build steps use today —
no Dir, no Net, no Clock, no imports beyond pure stdlib), whose only output
channel is *new items* (functions, consts, impls, types) returned as source
and appended to the module.

- Additive-only is structural, not policed: the API returns items; there is
  no handle to existing code. Footprint analysis runs after expansion, so
  generated code that demands authority shows up in `witchy caps` like
  anything else.
- Deterministic by construction (zero-ambient sandbox + no clock), so
  expansion is cacheable exactly like deterministic build steps.
- Use cases this unlocks without breaking the security story: lookup tables
  computed at compile time, derived serializers beyond the built-in
  derives, schema-to-record generation in-module (today only the
  build-step machinery can do this, at rune granularity).

Phase 5 ships only after 0–4 are stable; its design is fixed (sandboxed,
additive, post-expansion analysis) but its surface syntax can wait.

## Sequencing and the immediate hotfixes

The learning-log bugs don't wait for the phases:

1. **Now**: unknown-type `==` becomes a loud codegen error (F15 stops being
   silent); `dict.get` family gets content-equality backing; the
   runtime-built-key parity tests land.
2. **Now**: `Some`/`None` constructor availability made uniform (F16), even
   if the general fix re-lands cleaner with Phase 0.
3. Phase 0 (typed lowering) → Phase 1 (equality, completing the hotfix
   properly) → Phase 2 (stdlib) → Phase 3 (self) → Phase 4 (derive) →
   Phase 5 (comptime).

Phases 2 and 3 are breaking changes, taken in one cut each: witchy is
pre-prod, and carrying aliases, dual namespaces, or transition releases is
cruft with no constituency. The migration vehicle is `witchy fmt` (one run
mechanically rewrites a tree to the new spellings) plus removal errors that
name the new spelling — never a compatibility layer.
