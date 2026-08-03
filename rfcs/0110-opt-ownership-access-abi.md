---
rfc: 0110
title: "Opt ownership/access ABI completion"
status: accepted
created: 2026-08-01
updated: 2026-08-03
tracking: "Design accepted; no open questions. Foundation for RFC-0111/0112; no new source syntax. Criteria 1,3,4,5,7 PROVEN (checked logical access envelope, access matrix, envelope-erasure rejection, checked-place facts, tail-dispatch WIR); criteria 2,6,8,9,10 PARTIAL per rfcs/0110-0112-acceptance-ledger.md. Remaining before implemented: general normal-mode one-copy repair, direct-storage `var` lowering with the six proofs, paired normal-repair test cases, and real boundary-reown/ownership-token/direct-storage counters (currently placeholders)."
predecessors:
  - "[0024](0024-unified-facts-lattice.md) (shared ownership, escape, and liveness facts)"
  - "[0026](0026-unique-qualifier.md) (`unique` and `local unique` contracts)"
  - "[0033](0033-place-based-uniqueness.md) (place-based uniqueness and the direct own ABI)"
  - "[0083](0083-opt-mode-lifetimes.md) (read-only loans and lifetime-bearing function types)"
  - "[0087](0087-fused-mutators.md) (uniform `var` move-in/write-back semantics)"
  - "[0088](0088-ownership-aware-extraction.md) (capacity-token no-copy contracts)"
  - "[0090](0090-proper-tail-calls.md) (typed callable ABI convergence)"
related:
  - "[0111](0111-cross-boundary-specialized-layouts.md) (consumes the completed ownership ABI)"
  - "[0112](0112-borrowed-aggregate-types.md) (carries owner relations through aggregates)"
---

# RFC-0110: Opt ownership/access ABI completion

> This RFC adds no source syntax. Examples use the ownership conventions and
> qualifiers Witchy already ships.

## Summary

Complete Witchy's ownership ABI so the same checked access contract survives
every callable boundary: direct functions, methods, trait dispatch, closures,
function values, compiler-generated adapters, and proper-tail dispatchers.

Today direct calls can transport the hidden ownership/capacity token used by
`own` pipelines and selected `var unique` collection operations. First-class
calls cannot transport every token shape, and source-facing uniqueness
enforcement is intentionally narrower than the surface type suggests. A program
can therefore express a no-copy contract that the compiler must reject at an
indirect boundary even when the value is in fact unique.

This RFC makes the contract uniform:

- `let` is shared read access and never creates an owning ABI obligation;
- `var` is exclusive move-in/write-back access to a caller place;
- `own` transfers the caller's ownership state into the callee;
- `unique` requires a statically unique value at an opt call site;
- `local unique` additionally forbids escape from the activation; and
- a `unique` result returns the value together with its ownership state.

Normal mode keeps the copy-correct repair path. `mode opt` turns a missing
ownership proof into an error with the exact alias, loan, move, or ABI erasure
that caused it. Values and traps do not change; only copies, allocations, and
accepted performance contracts do.

## Motivation

### The source model is already stronger than the first-class ABI

Witchy's function types preserve `let`/`var`/`own`, concrete scalar/reference
kinds, and borrowed-result lifetime relations. Direct `own` calls also thread a
hidden capacity token so `x = grow(move x)` can stay in place across the call.
Direct functions returning a `unique List` or `unique Dict` can return that token
to a receiving binding.

The remaining split is specifically ownership metadata:

- a function value whose `var unique` parameter needs a collection token is
  rejected in `mode opt`;
- adapters can preserve the value signature while losing ownership state;
- `unique` checking is strongest for the measured `var unique` operations, not
  yet a general rule for every source-facing parameter; and
- an indirect tail edge may preserve the value/reference ABI without preserving
  the no-copy resource contract.

Those are ABI limitations, not inherent limits of value semantics.

### `var` is Witchy's exclusive access convention

Adding a second `&mut`-like parameter family would duplicate semantics Witchy
already has. A `var` argument:

1. names a mutable caller place;
2. reserves that place against overlapping write-back arguments;
3. evaluates in source order;
4. moves the current value into the callee; and
5. commits the final value only on structured return.

That is an exclusive access. In normal mode it may be implemented by a value
move plus reconstruction. In `mode opt`, a proven-unique place may be accessed
through its existing storage with no copy or reconstruction. This is a lowering
choice, not a new reference value and not new observable aliasing.

### Ownership metadata must compose before layouts do

RFC-0111 needs to pass packed buffers and caller-provided destinations across
calls. RFC-0112 needs function values and aggregates to retain owner relations.
Neither should invent a private call protocol. One exact ownership/access ABI is
the dependency both consume.

## Design

### One typed access signature

Every callable has a checked access signature derived from its existing function
type. For each parameter it records:

| Source contract | Access kind | Ownership input | Result obligation |
|---|---|---|---|
| ordinary `T` | owned immutable value | ordinary owned value | none |
| `let x: T` | shared borrow | no ownership transfer | cannot escape unless the typed result names the loan |
| `var x: T` | exclusive write-back | value plus current ownership state | final value plus final ownership state |
| `own x: T` | consuming transfer | value plus current ownership state | caller binding is dead |

Qualifiers refine that access:

- `unique T` requires `Unique` in the facts lattice at entry;
- `local unique T` requires `Unique` and an activation-bounded escape fact;
- `frozen T` permits sharing but rejects `var` and `own`; and
- `View(T, 'a)` carries RFC-0083's owner relation and never carries an owning
  object token for the viewed address.

The access signature is part of function-type identity. A cast, ascription,
trait witness, or generated adapter that would erase a convention, uniqueness
requirement, write-back result, ownership token, or loan relation is rejected.

### Representation-classed ownership state

The ownership state is typed by representation, not an untyped integer appended
to every call:

- scalars and zero-representation capabilities need no state;
- an owning linear-memory object carries the RC/capacity state its allocator and
  in-place operations require;
- a typed Wasm GC reference carries only the static uniqueness/access fact unless
  its concrete representation has a separate capacity-bearing child;
- a borrowed view carries an owner-root relation, never an owning-object token;
  and
- a packed value uses RFC-0111's layout-specific ownership state.

The checker and WIR encoder derive this classification from the same checked
type/layout descriptor. No string-shaped ABI table and no per-method token rule
is permitted.

### Direct and indirect calls use the same logical envelope

The logical ABI for a callable is:

```text
(explicit arguments, ownership inputs)
    -> (ordinary result, var write-backs, ownership outputs)
```

The physical Wasm signature may flatten or omit empty components. Direct calls,
typed closure-table entries, trait witnesses, existential adapters, and mutual
tail dispatchers must encode the same logical envelope. Generated adapters are
allowed only when they preserve every component exactly.

A closure wrapper remains a value containing code plus environment. Its callable
type selects an exact table signature. Two function types that differ in `var`,
`own`, uniqueness requirements, returned ownership state, or borrow relation do
not share an erasing table entry.

### General `unique` enforcement

At every source-facing call, the checker validates a `unique` parameter against
the ownership facts at that exact argument expression:

- a fresh value is unique;
- a binding receiving a direct `unique` result is unique;
- an `own` pipeline transports uniqueness;
- a field/index place is unique when the place oracle proves its root and path
  exclusive;
- an active view loan makes the owner non-unique;
- a live whole alias makes it shared; and
- an unknown or erased callable never guesses.

In normal mode, a missing proof inserts one explicit re-own copy before the call
and produces a fresh ownership state. In `mode opt`, it is a compile error. The
normal-mode repair is forbidden when the source contract is `local unique` and
the repaired value would escape the activation.

### `var` lowering as exclusive place access

The source semantics of `var` do not change. The optimizer may lower a `var`
argument onto caller storage when all of these hold:

1. the place and every projection coordinate are evaluated once;
2. the overlap checker proves it disjoint from every simultaneously reserved
   `var` place;
3. no live alias or view can observe intermediate storage;
4. the callee cannot escape the access;
5. the physical representation is identical on both sides; and
6. every structured return produces a valid final value and ownership state.

Otherwise normal mode uses move-in/write-back. `mode opt` reports which proof is
missing. A trap does not commit source-level write-back; compiled execution
already treats a trapped VM as terminal, and tests must ensure no host API can
resume or inspect a partially committed instance.

### Tail calls

An edge is proper only when the entire access envelope is forwarded without
residual reconstruction, drop, loan cleanup, or token repair. RFC-0090's
simultaneous rebind rule extends to ownership inputs and `var` outputs. An edge
that preserves the value signature but not the resource envelope remains an
ordinary call.

### Normal and opt boundaries

`mode opt` remains transitive across user imports. A normal caller may call an
opt API:

- an owned value may satisfy `let`;
- a unique value may satisfy `unique` directly;
- a shared value may be copied once to satisfy `unique`; and
- a borrowed result retains its declared loan in the normal caller.

An opt caller may call a normal function only through an already-permitted
standard-library boundary or a typed adapter whose summary proves the required
contract. This RFC does not weaken the existing transitive-import rule.

## Compiler architecture

The access signature is computed after type checking and method/trait
resolution, then attached to the checked callable identity. A single query API
serves:

- source call checking;
- ownership/escape analysis;
- closure and witness table construction;
- WIR signature selection;
- direct/indirect/tail lowering;
- opt-mode diagnostics; and
- deterministic resource statistics.

The implementation must not add a second AST-shape ownership engine. If the
current structural facts cannot answer a path-sensitive question, the work moves
that fact onto the shared CFG/SSA substrate rather than adding a call-specific
recognizer.

## Diagnostics

An opt-mode rejection names:

1. the required contract (`unique`, exclusive `var`, returned token, or loan);
2. the argument/place and callable;
3. the exact invalidating event;
4. whether the failure is an ownership proof or an unsupported physical ABI;
   and
5. a source-level repair when one exists (`move`, end/materialize a view, remove
   an alias, make the callee direct, or accept an owned copy in normal mode).

Diagnostics must never expose compiler-generated token local names.

## Verification

### Value and error parity

Every accepted program runs under:

- the interpreter;
- optimized compiled Wasm;
- `WITCHY_OPT=none`; and
- each relevant single-lever de-optimization.

Outputs, errors, argument evaluation order, write-back order, and use-after-move
diagnostics agree. An independent expected result accompanies parity tests so a
common frontend defect cannot pass by agreement.

### ABI shape

WIR tests inspect direct, closure, trait, existential, generated-adapter, and tail
signatures. Each test asserts the exact value/write-back/ownership components and
rejects an erasing function-value ascription.

### Resource facts

`witchy stats` gains or reuses deterministic counters for:

- boundary re-own copies;
- ownership-token repairs;
- direct-storage `var` accesses;
- indirect ownership-envelope calls; and
- destination candidates forwarded to RFC-0111.

An accepted `mode opt` no-copy call must record zero boundary re-own copies. A
normal-mode aliasing case must record exactly one repair and remain value-equal.

## Acceptance criteria

1. One typed access-signature representation covers direct functions, methods,
   traits, closures, function values, generated adapters, and tail dispatchers.
2. Every source-facing `unique` parameter is checked at every call shape; normal
   mode repairs by one copy and opt mode rejects a missing proof.
3. `var unique` and `own unique` collection state survives indirect calls and
   returns without an ABI-specific rejection.
4. Function-value ascription cannot erase conventions, uniqueness, write-back
   ownership outputs, or RFC-0083 lifetime relations.
5. Nested field and index places use the shared place/overlap oracle; two proven
   disjoint `var` projections work and overlapping or unknown projections reject.
6. Direct-storage `var` lowering preserves source evaluation and structured
   write-back semantics on every non-trapping path.
7. Proper-tail lowering forwards the full ownership envelope or declines the
   optimization; it never leaves hidden token repair after the tail edge.
8. Interpreter, optimized Wasm, forced-copy Wasm, and the independent expected
   oracle agree across the direct/indirect/trait/closure/place matrix.
9. Deterministic counters prove zero boundary copies for accepted opt contracts
   and exactly one repair for the paired normal-mode cases.
10. `spec/language.md`, `spec/performance.md`, callable reflection, compiler
    diagnostics, and the runnable book describe the same access contract.

The RFC remains `proposed` until all ten criteria are proven in a checked
acceptance ledger. Partial call-shape coverage is not implementation completion.

## Staging and dependency graph

1. **Access signature and verifier.** Introduce the checked representation and
   reject erasure before changing code generation.
2. **Physical ABI convergence.** Direct, closure, trait, and generated adapters
   encode the envelope; differential tests stay green after each call family.
3. **General unique checking.** Promote every source-facing parameter and add
   normal-mode one-copy repair.
4. **Exclusive-place lowering.** Consume the existing overlap/place facts; move
   missing precision to the shared facts substrate.
5. **Tail and resource proof.** Extend proper tails and land deterministic
   counters plus the acceptance ledger.

RFC-0111 and RFC-0112 depend on stages 1–2. After those interfaces freeze, their
implementation tracks may proceed independently.

## Alternatives

- **Add `&` and `&mut`.** Rejected. `let` and `var` already express shared and
  exclusive access without making references first-class values or changing
  Witchy's value semantics.
- **Keep indirect calls copy-correct only.** Safe, but turns abstraction into a
  permanent performance cliff and prevents layout/destination contracts from
  composing.
- **Use runtime `rc == 1` as the only proof.** Useful as a normal-mode repair,
  but insufficient for `mode opt`: a missed compile-time proof must be loud, and
  views/typed GC references are not interchangeable with owning RC object bases.
- **Add per-container tokens.** Rejected. Representation-classed ownership state
  is derived from checked types and layouts; a new container must not require a
  new calling convention.

## Drawbacks

- Function-type identity and table specialization become stricter, increasing
  the number of physical signatures and potentially compiled code size.
- Normal-mode one-copy repair exposes an otherwise implicit performance cliff in
  the IR and statistics, but it does not remove the cost.
- Moving ownership facts onto shared CFG/SSA infrastructure is broader than a
  local ABI patch and may delay individual call-shape fixes.
- Exact adapter verification reduces the set of convenient but lossy function
  ascriptions. Diagnostics must make the rejected resource relation legible.

## Prior art

- [Implementation Strategies for Mutable Value
  Semantics](../external-refs/mutable-value-semantics-2022/notes.md) supplies the
  `let`/`inout`/`sink` model that Witchy's `let`/`var`/`own` conventions adapt.
- [Perceus](../external-refs/perceus-2021/notes.md) demonstrates precise RC and
  reuse as one compositional ownership discipline rather than container-specific
  calling conventions.
- [Fully in-Place Functional
  Programming](../external-refs/fip-fully-in-place-2023/notes.md) is the static
  zero-allocation counterpart to the no-copy contracts completed here.

## Non-goals

- No new source syntax.
- No shared mutable reference values or pointer identity.
- No relaxation of capability or Wasm sandbox boundaries.
- No claim that ABI completion alone reaches Rust-class throughput; RFC-0111
  supplies representation and measurement.
- No silent opt-mode fallback when a proof or ABI component is missing.
