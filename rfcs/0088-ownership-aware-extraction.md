---
rfc: 0088
title: "Ownership-aware update and extraction without mandatory container copies"
status: proposed
created: 2026-07-14
predecessors:
  - "0087 (uniform var write-back - defines the source semantics this RFC optimizes)"
related:
  - "0033 (place-based uniqueness - supplies ownership facts through user types)"
  - "0051 (memory-safety invariants - existing in-place paths are the performance floor)"
  - "0029 (performance-tier contract - normal mode copies; opt mode may reject)"
  - "0083 (opt-mode lifetimes - active loans prevent in-place update or extraction)"
tracking: prototype and instrumented dictionary-probe report required before acceptance
---

# RFC-0088: Ownership-aware update and extraction

## Summary

Add a general ownership-aware lowering mechanism for operations that update a
`var` container and independently return a value selected from its old state.
The first three operations are deliberately structurally different:

```
List.pop(var self) -> Option(a)
Dict.remove(var self, key) -> Option(v)
Dict.insert(var self, key, value) -> Option(v)
```

`List.pop` and `Dict.remove` remove and return an old leaf. `Dict.insert` either
adds a new leaf and returns `None`, or replaces an existing leaf and returns
`Some(old_value)`. All three use RFC-0087's existing source semantics. This RFC
changes only how the ordinary result and final `var` value are produced.

A correct baseline may copy. An optimized implementation may reuse uniquely
owned storage, but it must produce the same ordinary result and final container.
No O(1), single-lookup, no-copy, or allocation-free claim ships until the
prototype and acceptance gates below prove that exact operation/mode/ownership
combination.

The RFC does not assume that one high-level WIR instruction can express every
container algorithm. It requires a prototype comparing a low-level move-out
ownership primitive with a descriptor-driven update-and-extract instruction.
The selected mechanism must centralize ownership decisions without hiding a new
container-kind or method-name census inside an allegedly generic opcode.

## Motivation

Uniform `var` supplies the right ABI:

```
var scores = dict.new()
let first = scores.insert("ada", 36)   // None
let old = scores.insert("ada", 37)     // Some(36)
```

The ordinary `Option` result and write-back to `scores` are independent. The
call can also stand alone when the displaced value is irrelevant. No special
syntax or return-shape classification is needed.

The ABI alone does not make the operation cheap. At the RFC-0087 landing commit
`048dead4`, public `Dict.insert` computes `previous = get(d, key)` and then calls
`dict.__insert`. `get` walks `dict.pairs`, while the insert helper performs its
own table search. The existing `dict_insert_cap` path preserves amortized O(1)
mutation of a clean uniquely owned dictionary, but it does not return the
displaced value. The source API is right; the displaced-value implementation is
not a fused, single-lookup operation.

`List.pop` and `Dict.remove` have the same ownership problem in different
structural algorithms. The implementation must select an old leaf, return it
exactly once, repair or replace the container representation, and retain or
release every other leaf correctly under unique and shared roots. Solving only
one named method would continue the per-method helper family that RFC-0051 keeps
only as a measured compatibility bridge.

The current in-place family remains load-bearing. RFC-0051 measured
out-of-memory and multi-fold regressions when those paths miss. RFC-0087
therefore preserved and re-keyed them during its semantic cut. This RFC is the
separately measured general replacement path, not permission to delete working
optimizations before equivalent coverage exists.

## Scope

### Operation family

An update-and-extract operation has three semantic outputs:

1. whether a projection was present;
2. the selected value when present;
3. the final container written back through `var`.

The structural action may be:

- **remove:** delete the selected projection (`List.pop`, `Dict.remove`);
- **replace:** install a new value and return the selected old value
  (`Dict.insert` on an existing key);
- **insert-on-miss:** install a value with no selected old value
  (`Dict.insert` on an absent key).

Future operations such as an atomic value exchange may use the same ownership
primitive, but they are not required for acceptance.

### Non-goals

- No source-language change, transaction syntax, rollback rule, or new
  collection method is proposed.
- No hash-table representation or ordering change is proposed.
- This RFC does not make arbitrary `var` calls automatically in-place; it
  defines the ownership substrate used by structural collection algorithms.
- Fully in-place functional kernels remain RFC-0089 research.
- Async `var` parameters and returned borrowed views remain outside this RFC.

## Semantic contract

RFC-0087 remains the sole source-level contract. Each call returns its ordinary
`Option` and commits the final container on every structured return. This RFC
may only choose the lowering that produces that result pair.

The forced-copy implementation is the value oracle. Unique, shared, optimized,
interpreter, and compiled executions must agree on:

- presence, extracted value, and empty/missing behavior;
- final contents and insertion order;
- trap and structured-return behavior;
- alias visibility: an old shared root remains unchanged;
- drop counts and externally observable resource ownership.

For example:

```
var d = dict.from_pairs([("a", payload)])
let snapshot = d
let old = d.insert("a", replacement)
```

`snapshot` still contains `payload`; `d` contains `replacement`; and `old` is
`Some(payload)`. An optimization may mutate storage only after proving that no
such live alias or loan exists.

## Lowering model

### Three layers

Update-and-extract lowering separates three concerns:

1. The typed call layer identifies the ordinary result, every final `var`
   value, and each write-back place. It does not inspect method names.
2. A structural algorithm locates the list slot or dictionary entry and
   supplies the projection plus remove/replace repair action. Different data
   structures may inherently need different algorithms.
3. An ownership primitive decides whether storage can be reused, moves or
   retains the selected leaf exactly once, and performs required retain/drop
   actions for the repaired representation.

Only layer 3 must be general. Layer 2 may use list and dictionary algorithms,
but it must identify a dictionary entry once and reuse that result for the
ordinary return and structural update. `get` followed by `insert`,
`contains_key` followed by `get_or`, or any other second key search does not
satisfy this RFC.

### Candidate WIR boundaries

Before acceptance, two WIR designs are prototyped:

- **Move-out primitive.** A low-level operation takes a proven initialized
  place in unique storage, transfers its leaf, and leaves an initialized repair
  value. Existing structural lowering composes presence tests, list metadata,
  and dictionary table repair around it.
- **Descriptor-driven update-and-extract.** One ownership operation consumes a
  layout descriptor, selected projection, replacement-or-remove action, and
  repair metadata supplied by typed structural lowering. The descriptor carries
  layout and ownership facts, never a source method or container type name.

Both candidates are exercised on `List.pop`, replacing `Dict.insert`, and
`Dict.remove`. The prototype records generated WIR, ownership facts, code size,
runtime helper count, key-probe count, retain/drop behavior, and the work needed
to add a fourth structural algorithm. The smaller mechanism that centralizes
ownership without obscuring structural logic becomes normative in a revision
of this proposed RFC.

### Dictionary single-search contract

The dictionary structural layer returns a search result that includes at least
presence and the entry location needed by the update. That one result drives:

- construction of `None` or `Some(old_value)`;
- replacement, removal, or insertion;
- insertion-order metadata repair;
- retain/drop decisions for the key and old/new values.

The implementation must include an instrumented test oracle that counts
structural search invocations independently from key comparisons. A collision
may require several comparisons within one search, so comparison count alone
cannot prove the single-search property. Replacement and removal of a present
key perform one structural search; insertion of an absent key performs one
complete miss search and reuses its insertion position. A resize may rehash
entries but must not repeat semantic key lookup through the public dictionary
API. Wall-clock benchmarks alone are not acceptance evidence for this rule.

## Ownership behavior

In normal mode:

- proven unique storage may update in place and move the selected leaf out;
- shared or unknown storage takes a correct copy-on-write path;
- a missed uniqueness fact costs a copy, never a semantic change or rejection.

For unique storage, successful extraction transfers the old leaf into the
ordinary result without an avoidable retain/drop pair. For shared storage, the
old root remains unchanged and the returned leaf gains exactly the ownership
needed to outlive both old and new containers. On a miss, no uninitialized or
sentinel leaf may escape through `None`.

In `mode opt`, an API that promises no-copy update-and-extract requires a
`unique` or `local unique` proof. A miss is a compile error under RFC-0029 and
reports why the owner is shared. The `why_not` diagnostic vocabulary is
implementation work, not a current facility assumed by this RFC.

An active read-only loan from RFC-0083 makes the owner unavailable for in-place
update or extraction even when its runtime reference count is one. Normal source
already rejects mutation while such a view is live; optimization analysis
consumes the same typed loan fact and cannot override it.

## Compatibility with current fast paths

This RFC is additive:

- existing `*_cap` and `self_*` behavior remains until measured replacements
  cover the same operation and pass RFC-0051's gates;
- `WITCHY_OPT=-inplace` remains the forced-copy differential control;
- no source method name becomes an ownership-analysis key;
- no old path is removed because the prototype works for only one shape;
- `Dict.insert` may keep its existing mutation fast path until the new path
  also returns the displaced value with equal or better evidence.

## Memory invariants

Every update-and-extract lowering proves:

1. a present selected leaf is initialized and returned exactly once;
2. an absent projection returns `None` without reading a leaf slot;
3. the repaired container is initialized on every non-trapping path;
4. old shared storage is not mutated;
5. overwritten or abandoned storage is dropped exactly once;
6. returned heap values remain live independently of old and new containers;
7. capacity, length, hash index, insertion order, and projection metadata remain
   valid after empty, singleton, replace, remove, insert, resize, and miss cases;
8. early returns and traps cannot expose a partially committed multi-result;
9. active view roots and host leases outlive every borrowed projection.

## Implementation order

1. Preserve RFC-0087's copy-correct source behavior and RFC-0051 fast paths.
2. Add an instrumented dictionary search/key-comparison oracle before changing
   `Dict.insert` or `Dict.remove` lowering.
3. Prototype both WIR boundaries for `List.pop` and replacing `Dict.insert`,
   including scalar and heap-valued leaves, forced-copy, and aliasing tests.
4. Apply each candidate to `Dict.remove`; reject a design that needs hidden
   method-name or container-kind dispatch in the ownership layer.
5. Select and specify the lower-level primitive using the prototype report.
6. Land the normal-mode unique and copy-on-write paths for all three operations.
7. Enable opt-mode no-copy diagnostics only after ownership explanations and
   view-loan facts are available.
8. Retire an old helper only after its complete benchmark and memory-safety
   matrix passes through the replacement.

## Acceptance criteria

1. The prototype report compares both candidate WIR boundaries on `List.pop`,
   `Dict.remove`, and replacing/missing `Dict.insert`, and explains the selected
   boundary between structural and ownership logic.
2. All three operations pass interpreter/compiled and forced-copy differential
   tests for empty/missing, singleton, multi-element, nested, shared, and
   proven-unique containers.
3. `Dict.insert` and `Dict.remove` pass an instrumented one-search gate for
   present and absent keys. The test distinguishes semantic lookup from resize
   rehashing and fails the current `get`-then-update baseline.
4. Replacing `Dict.insert` returns the exact displaced scalar or heap value;
   missing insert returns `None`; both leave insertion order unchanged from the
   source contract.
5. Refcount, heap-bound, canary, early-return, and trap tests prove the memory
   invariants for scalar, string, nested-container, record, and user-ADT leaves.
6. The same ownership primitive serves list and dictionary structural lowering
   without source method recognition or a central container-kind switch.
7. Shared roots copy in normal mode; proven unique roots avoid a full-container
   copy; promised no-copy sites reject in opt mode with a useful explanation.
8. Active RFC-0083 loans block the in-place path and cannot cause use-after-free
   or mutation-through-view.
9. RFC-0051's memory-cliff kernels still complete. `list_index`,
   `binary_trees`, and `expr_eval` remain within their recorded threshold, and
   dedicated pop, remove, missing-insert, and replacing-insert workloads record
   allocations, copied bytes, key comparisons, and elapsed time.
10. The uniquely owned replacing-insert benchmark demonstrates amortized O(1)
    table update, one semantic key search, no full-container copy, and no
    avoidable retain/drop pair for the displaced value.
11. Documentation publishes complexity claims only for measured combinations
    of operation, ownership proof, key mode, and performance mode.

## Alternatives

- **Keep `get` followed by update.** This is the current copy-correct baseline,
  but it makes displaced-value insertion perform separate selection and update
  work and cannot satisfy a single-search contract.
- **Add `pop_cap`, `dict_remove_cap`, `dict_replace_cap`, and peers.** Direct,
  but extends the method-specific family RFC-0051 permits only as a measured
  bridge.
- **Put every container behind one extraction opcode.** Uniform spelling is not
  a uniform mechanism if the opcode contains a relocated type census.
- **Return the old value only in opt mode.** Rejected: performance modes cannot
  change source semantics or ordinary result types.
- **Require uniqueness in normal mode.** Gives predictable performance but
  violates normal mode's copy-correct fallback contract.
- **Bundle extraction into RFC-0087.** RFC-0087 is implemented and owns source
  semantics. Reopening it would couple that coherent model to an unproven
  optimization representation.

## Drawbacks

- The prototype may show that only the ownership leaf operation is general;
  structural algorithms will still require type-specific implementations.
- Keeping existing paths during transition temporarily increases compiler and
  runtime complexity.
- Normal-mode complexity remains ownership-sensitive even when values are
  semantically identical.
- The one-search contract requires instrumentation in addition to ordinary
  differential and benchmark tests.
- Useful opt-mode errors depend on facts and diagnostics not yet implemented.

## Prior art

[Perceus](../external-refs/perceus-2021/) and Lean's reuse analysis motivate
reuse under precise ownership. Hylo's mutable value semantics motivate keeping
the optimization unobservable. Swift's consuming and borrowing work informs
move-out safety. Rust's `HashMap::insert` and entry APIs illustrate a
single-search replacement that returns displaced values, while its collection
implementations also show why structural algorithms cannot be collapsed into
one container-agnostic operation.
