---
rfc: 0047
title: "One equality: == through PartialEq at every depth"
status: implemented
created: 2026-07-03
tracking: "merged to master 7ee6323"
---

> 2026-07-03: implemented and merged (7ee6323). == desugars through PartialEq at
> every depth; == on function/capability types is a compile error (deleting the
> f == f parity divergence); dict/set keys require Eq; bytes.at OOB fixed (SEC-038).
> Behavior lives in spec/language.md.

# RFC-0047: One equality: `==` through PartialEq at every depth

The shipped equality rules are specified in
[`spec/language.md`](../spec/language.md), with type-checking/lowering coverage in
[`crates/witchy-types/src/typeck.rs`](../crates/witchy-types/src/typeck.rs) and
[`crates/witchy-lower/src/codegen`](../crates/witchy-lower/src/codegen), plus parity
regressions in [`src/example_tests.rs`](../src/example_tests.rs).

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not compile pre-implementation
> snippets.

## Summary

Witchy currently has *several* equalities wearing one operator. This RFC makes
`==`/`!=` desugar through `PartialEq` uniformly, at every depth: a custom
`impl PartialEq for P` is honored whether `P` is compared at top level, inside
a `List(P)`, an `Option(P)`, a tuple, or a `Dict` value. `derive(PartialEq)`
(and the existing `Eq` derive) gives the structural behavior, so types without
a custom impl are bit-for-bit unchanged. `==` on function types and capability
types becomes a **compile-time error** — which fixes a live, confirmed backend
parity violation by construction. `Dict`/`Set` keys require `Eq`, closing the
NaN-key hole. The spec's equality claims ([`spec/language.md:193, :1032`](../spec/language.md)) are
corrected as part of implementation.

## Motivation

All of the following were re-probed against the shipped binary on 2026-07-03.

**1. Custom equality silently vanishes below the surface.** With
`impl PartialEq for P: fn eq(self, other) -> Bool: true` (an always-true
impl):

```
P(1) == P(2)             // true  — the impl is called
[P(1)] == [P(2)]         // false — structural memcmp, impl ignored
Some(P(1)) == Some(P(2)) // false
(P(1), 0) == (P(2), 0)   // false
```

Parity-consistent, so no backend warns — this is exactly the
silent-misbehavior class the project forbids. The cause is visible in
`operator_dispatches` ([`crates/witchy-types/src/traits.rs:597`](../crates/witchy-types/src/traits.rs)): the operator
desugars to the trait method only when the *operand's own head type* has an
impl; a container head (`List`, `Option`, tuples — tuples are even explicitly
excluded at traits.rs:604) keeps the native structural path, which never
consults element impls (the compound-shape helper in
[`crates/witchy-lower/src/codegen/mod.rs:4514`](../crates/witchy-lower/src/codegen/mod.rs) is pure structure).

**2. The spec is wrong on both counts.** spec/language.md:193 claims `==` is
"**structural** equality — deep … on every backend." It is not structural at
the top level when an impl exists, and not impl-honoring below it. Whichever
semantics we pick, that line is false today.

**3. Named-function `==` is a confirmed parity violation.** `fn f …; f == f`:
interpreter says `true` (name/identity), compiled says `false` (closure-pointer
compare) — `witchy parity` itself reports "the two backends DIVERGE." Function
and capability equality are specified nowhere (the spec's operator table and
§16 parity contract simply don't mention them).

**4. NaN is accepted as a dict key** (probed: `d[0.0/0.0] = "nan"` then
`d.get_or(0.0/0.0, "missing")` → `"missing"`, both backends) — an unretrievable
entry, despite [`std/cmp.witchy:27–29`](../std/cmp.witchy)'s own doctrine: "Types usable as `Set` /
`Dict` keys are `Eq`" and Float is explicitly *not* `Eq`.

**5. Stale spec claim the other way**: spec/language.md:1032 says
multi-parameter generic payloads (`Result`) are "the one compile-time-rejected
comparison" — probed: `Ok(1) == Ok(1)` compiles and runs `true` on both
backends.

One operator, five stories. The cmp hierarchy RFC built `PartialEq → Eq →
PartialOrd → Ord` precisely so operators would have *one* trait-backed meaning;
equality is the half where that program was left unfinished.

## Design

### 1. `==`/`!=` are PartialEq, all the way down

- `a == b` requires `T: PartialEq` for the operands' shared type `T` and means
  `PartialEq.eq(a, b)`; `!=` is `ne` (default `!eq`, as std/cmp.witchy already
  defines).
- **Built-in containers get real PartialEq semantics over their elements**:
  `List(a)`, `Option(a)`, `Result(a, e)`, tuples, records, `Dict(k, v)`
  (insertion-order-sensitive, as today), `Set(a)`, `Bytes` — each compares via
  its elements'/fields' PartialEq, recursively. A custom impl is therefore
  honored at every depth. Conceptually these are the blanket impls
  (`impl PartialEq for List(a) where a: PartialEq`); implementation-wise the
  backends keep their compound-equality helpers and add one rule: when
  lowering a compound shape whose element type has a *user* PartialEq impl in
  the linked program, the generated helper calls the impl for that element
  instead of recursing structurally.
- `derive(PartialEq)`/`derive(Eq)` generate the structural impl (they already
  exist); a type with neither a derive nor a hand impl gets the implicit
  structural behavior it has today. **No existing program without a custom
  `PartialEq` impl changes behavior** — the change is only that custom impls
  stop being depth-dependent.

### 2. `==` on function and capability types is a compile-time error

```
error: `==` is not defined on function types — there is no meaningful
       equality for functions (identity is not stable across compilation).
       Compare the values functions *produce*, not the functions.
```

Rationale: there is no stable semantic to standardize. Identity equality
breaks under monomorphization/inlining (the "same" function is many pointers,
or two functions fold into one); structural equality is undecidable. Rust
reached the same conclusion (closures don't implement `PartialEq`; comparing
fn pointers is `unpredictable_function_pointer_comparisons`-linted). Any
runtime answer must be either backend-dependent (today's live bug) or a
forever-specified arbitrary rule. Rejecting is also the only option consistent
with §16's "loud error, never a silently different answer."

Same rule for capability types (`Console == Console` type-checks today with no
specified meaning): capabilities are authority, not data; asking whether two
authorities are "equal" has no answer the type system should pretend to have.

This **fixes the parity violation by construction** — the divergent code paths
(interpreter name-compare vs compiled pointer-compare) are deleted, not
reconciled.

### 3. Dict/Set keys require `Eq`

The checker enforces `k: Eq` for `Dict(k, v)` keys and `Set` members — the
doctrine std/cmp.witchy:27–29 already states. Consequences:

- **Float keys are rejected** at compile time (Float is PartialEq, not Eq).
  This closes the NaN-key hole (#4) wholesale rather than special-casing NaN.
  The teaching error suggests the standard escapes: an Int key (scaled), or a
  String rendering.
- Reconciliation with the compiled backend's current surface: codegen today
  supports Int/Bool/Duration/String keys and rejects everything else at lowering
  ("could not determine the Dict key type for WASM; use Int, Bool, Duration, or
  String keys", codegen/mod.rs) while the interpreter accepts more (tuple and
  record keys run interpreter-only). The public `witchy check` path verifies
  compiled-backend acceptance, so these are loud check failures rather than
  check-passes-codegen-fails surprises. The
  Eq bound moves this whole decision to **one type-level rule in the checker**:
  Float leaves the key set; Int/String/Bool/Duration and Eq-deriving
  records/enums are admissible *in the type system*, with the compiled
  backend's key support extended to match (record/enum keys hash their
  canonical structural form). Interpreter-only key types drop to zero, per the
  minimize-interpreter-only-features rule.

### 4. Ordering operators follow (small, same shape)

`< <= > >=` already dispatch through `PartialOrd` for non-primitives
([`traits.rs:782–806`](../crates/witchy-types/src/traits.rs)). The one change for coherence: the same
depth-uniformity rule applies (a custom `PartialOrd` inside a compared
container is honored once containers gain ordering impls) — but **no new
container orderings are introduced by this RFC**; today's "ordering on
Int/Float/String/Duration only" stays. This clause exists so the equality fix
doesn't create an equality/ordering asymmetry in the trait story.

### 5. Spec corrections (part of the implementation, not follow-up)

- [`spec/language.md:193`](../spec/language.md) → "`==`/`!=` desugar through `PartialEq`; the derived/
  default impl is deep structural equality; custom impls are honored at every
  depth; function and capability types do not compare."
- [`spec/language.md:1032`](../spec/language.md) → drop the stale Result-rejection claim; state the
  function/capability rejection in the §16 parity contract instead.
- std/cmp.witchy doc-comments already say the right thing; they become true.

### 6. Performance: keep the structural fast path when no impl exists

The concern: deep trait-dispatched equality would turn today's tight
compound-equality helpers into virtual calls per element. The answer is a
**whole-program fact the linker already has**: after linking there is one
module and a complete impl table. If the linked program contains **no** custom
`PartialEq` impl for type `T` (derives generate structural impls, which are
recognized as such), then `==` over any shape containing `T` lowers to exactly
today's structural helper — the common case (the entire current corpus modulo
a handful of tests) compiles to identical code. Only shapes actually
containing a custom-impl type pay for dispatch, and they are getting semantics
they simply don't have today. This mirrors how `operator_dispatches` already
consults `lookup_impl` — the lookup moves from "operand head only" to "any
type embedded in the compared shape," computed once per shape at lowering.

### Interaction with sibling RFCs

- [RFC-0048](0048-fallback-operator.md) narrows `||` to Bool; together these
  two make the operator table honest.
- RFC-0046 (typed trait dispatch) replaces the string-shape dispatch
  machinery this RFC extends; the semantics here are defined
  machinery-independently so 0046 can land before or after.
- [RFC-0051](0051-memory-safety-invariants.md) owns the compiled helpers'
  memory discipline; the new impl-calling equality helpers follow its rules.

## Alternatives

- **Always-structural, custom impls banned from backing `==`** (impls callable
  only by name). Rejected: it forfeits the operator ergonomics the entire cmp
  hierarchy was built to provide ("`a == b` … work[s] on your own types once
  you implement (or derive) them" — std/cmp.witchy:3–4), and case-insensitive
  keys / cached-hash records / civil-time equality are real, legitimate custom
  equalities.
- **Top-level-only dispatch, documented** (spec today's behavior). Rejected:
  "your equality applies except inside any container" is not a semantics
  anyone can build on — it makes `xs.contains(x)` and `x == y` disagree, which
  is a bug factory, not a rule.
- **Function `==` as identity on both backends** (make the compiled answer the
  spec). Rejected: it must then be specified forever, it's still meaningless
  (monomorphization/inlining make identity an implementation accident), and
  the interpreter would need closure-identity plumbing purely to reproduce an
  accident. Rejecting costs the corpus almost nothing (zero non-test uses
  found in std/examples/projects).
- **NaN-only dict-key rejection at runtime** (allow Float keys, trap on NaN).
  Rejected: a runtime trap for a statically-knowable type error, and Float
  keys remain a precision trap (`0.1 + 0.2` as a key). The Eq bound is the
  principled rule the stdlib already documents.
- **Do nothing.** Keeps a confirmed parity violation, a false spec, and a
  silent-misbehavior edge in the language's most-used operator.

## Drawbacks

- **Breaking**: programs comparing functions or capabilities stop compiling
  (corpus scan found only the differential test that documents the divergence);
  Float dict keys stop compiling (probes found no in-repo non-test users, but
  external code may exist — pre-1.0, one cut, per break-don't-deprecate).
- **Compiled-backend work is real**: per-shape equality helpers that can call
  user impls mid-recursion, and record/enum dict keys, are new codegen surface
  — each needs differential tests and (per the parity rule) a book example.
- The "no custom impl ⇒ structural fast path" optimization is a whole-program
  argument; it must be covered by the WITCHY_OPT invariance sweep so the
  de-optimized build (dispatch everywhere) stays observably identical.
- Deep equality calling user code can now fail/abort mid-comparison (a user
  `eq` that traps); this is accepted — it is ordinary user code execution, and
  the same is already true of `less` in `sort_by`.

## Prior art

- **Rust**: `PartialEq`/`Eq` with derive + blanket container impls is exactly
  this shape; Rust also declines `Eq` for `f64` and warns on fn-pointer
  comparison (`unpredictable_function_pointer_comparisons`) — both decisions
  echoed here, the latter hardened to a hard error.
- **The comparison-hierarchy work** (std/cmp.witchy; the supertraits +
  type-directed operator rewrite recorded in project memory) — this RFC is
  that design finishing what it started.
- **Python**: `__eq__` honored at every depth inside lists/dicts — evidence
  the depth-uniform rule is what users of a "Python layout" language will
  assume; NaN dict keys are Python's own well-known trap, avoided here by the
  Eq bound.
- [RFC-0005](0005-unforgeable-capabilities.md): capabilities as opaque,
  non-data values — the same stance that makes capability `==` a type error.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
