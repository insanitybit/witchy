---
rfc: 0088
title: "Ownership-aware extraction without mandatory container copies"
status: proposed
created: 2026-07-14
predecessors:
  - "0087 (uniform var write-back - defines the source semantics this RFC optimizes)"
related:
  - "0033 (WIR ownership lowering - leaf operations may remain type-specific)"
  - "0051 (memory-safety invariants - existing in-place paths are the floor)"
  - "0029 (performance-tier contract - normal mode copies; opt mode may reject)"
  - "0083 (opt-mode lifetimes - active loans prevent in-place extraction)"
tracking: prototype required before acceptance
---

# RFC-0088: Ownership-aware extraction

## Summary

Add a general ownership-aware lowering mechanism for operations that update a
`var` container and independently return a value taken from that container. The
first target is `List.pop(var self) -> Option(a)` from RFC-0087.

This RFC changes no source semantics. A correct implementation may copy the
container. An optimized implementation may reuse uniquely owned storage, but it
must produce the same ordinary result and final `var` value. No O(1), no-copy,
or allocation-free claim ships until the prototype and acceptance gates below
prove it.

The RFC does not assume that one high-level WIR instruction can express every
container algorithm. It first requires a prototype comparing a low-level
move-out ownership primitive with a layout-descriptor extraction instruction.
The accepted WIR shape must centralize ownership decisions without disguising a
new container-kind or method-name census.

## Motivation

Uniform `var` supplies the right call ABI: a pop can return both `Option(a)` and
the final list. The ABI alone does not make extracting the element cheap. A
baseline implementation can preserve value semantics by copying the list,
removing the last element from the copy, and returning both values. Repeating
that operation is linear per pop.

The current in-place family is load-bearing. RFC-0051 measured out-of-memory and
multi-fold regressions when existing list, dictionary, and string paths miss.
RFC-0087 therefore preserves and re-keys those paths during its semantic cut.
This RFC is the separately measured replacement path, not permission to delete
working optimizations early.

Extraction also has a distinct ownership problem from ordinary update. The
implementation must move an element out, repair the container representation,
return the element exactly once, and release or retain every remaining leaf
correctly under unique and shared roots.

## Design

### Semantic contract

For a call such as:

```
let item = xs.pop()
```

RFC-0087 remains the sole source-level contract. The call returns an ordinary
`Option(a)` and commits the final list to `xs` on every structured return. This
RFC may only choose how that result pair is represented and produced.

The forced-copy implementation is the value oracle. Unique, shared, optimized,
interpreter, and compiled executions must agree on:

- the extracted value and empty-container behavior;
- the final container contents and order;
- trap and structured-return behavior;
- drop counts and externally observable resource ownership.

### Three layers

Extraction lowering separates three concerns:

1. The typed call layer identifies the ordinary result, each final `var` value,
   and the write-back place. It does not inspect method names.
2. A structural algorithm identifies the container slot, table entry, tree path,
   or other projection being removed and repairs that representation. Different
   data structures may inherently need different algorithms.
3. An ownership primitive decides whether storage can be reused, moves the
   extracted leaf exactly once, and performs the required retain/drop actions.

Only layer 3 is required to be general. Moving type-specific algorithms behind
one opcode whose implementation switches on container kind would merely relocate
the existing zoo and does not satisfy this RFC.

### Prototype gate

Before this RFC can be accepted, two WIR designs are prototyped for `List.pop`:

- **Move-out primitive.** A low-level operation takes a proven place/slot from
  unique storage, leaves an initialized repaired representation, and returns the
  moved leaf. Existing structural lowering composes it into pop.
- **Descriptor-driven extraction.** One extraction operation consumes a layout
  descriptor plus projection/repair data supplied by typed lowering. The
  descriptor contains layout facts, not a method or source-level type name.

The prototype records generated WIR, ownership facts, code size, runtime helper
count, benchmark results, and the work needed to add dictionary removal. The
smaller mechanism that keeps ownership logic centralized without obscuring
structural algorithms becomes normative in a revision of this proposed RFC.

### Ownership behavior

In normal mode:

- proven unique storage may use the in-place extraction path;
- shared or unknown storage takes the correct copy-on-write path;
- a missed uniqueness fact costs a copy, never a semantic change or rejection.

In `mode opt`, an API that promises no-copy extraction requires a `unique` or
`local unique` proof. A miss is a compile error under RFC-0029 and reports why
the owner is shared. The `why_not` diagnostic vocabulary is implementation work,
not a current facility assumed by this RFC.

An active read-only loan from RFC-0083 makes the owner unavailable for in-place
extraction even when its runtime reference count is one. Normal source already
rejects mutation while that view is live; optimization analysis consumes the
same typed loan fact and cannot override it.

### Compatibility with the current fast paths

RFC-0087's Phase-1 gate re-keys the current `x = f(x, ...)` recognizers to typed
`var` calls before deleting the old statement rewrite. This RFC is additive:

- existing `*_cap` and `self_*` behavior remains until measured replacements
  cover the same operation and pass the RFC-0051 gates;
- `WITCHY_OPT=-inplace` remains the forced-copy differential control;
- no new source method name becomes an ownership-analysis key;
- no existing path is removed merely because the prototype works for one list
  shape.

### Memory invariants

Every extraction lowering proves:

1. the extracted leaf is initialized and returned exactly once;
2. the repaired container remains initialized on every non-trapping path;
3. old shared storage is not mutated;
4. overwritten or abandoned storage is dropped exactly once;
5. capacity, length, and projection metadata stay valid after empty, singleton,
   success, and failure cases;
6. early returns and traps cannot expose a partially committed multi-result;
7. active view roots and host leases outlive every borrowed projection.

## Implementation order

1. Land RFC-0087 with its copy-correct baseline and preserved RFC-0051 paths.
2. Prototype both WIR shapes for `List.pop`, including forced-copy and aliasing
   tests, without deleting an existing optimization.
3. Select and specify the lower-level primitive using prototype evidence.
4. Extend the mechanism to `Dict.remove`; revise if this requires a hidden
   container-kind dispatch.
5. Enable opt-mode no-copy diagnostics only after ownership explanations and
   view-loan facts are available.
6. Retire an old helper only after its whole benchmark and memory-safety matrix
   passes through the replacement.

## Acceptance criteria

1. The prototype report compares both candidate WIR shapes and explains the
   selected boundary between structural and ownership logic.
2. `List.pop` passes interpreter/compiled and forced-copy differential tests for
   empty, singleton, multi-element, nested, shared, and proven-unique lists.
3. Refcount, heap-bound, canary, early-return, and trap tests prove the memory
   invariants above for scalar and heap-valued elements.
4. RFC-0051's `list_sum`, `list_index`, `binary_trees`, and `expr_eval` gates do
   not regress when list extraction is enabled or when an old path is retired.
5. A second structurally different operation, initially `Dict.remove`, uses the
   same ownership mechanism without method-name recognition or a central
   container-kind switch.
6. Shared roots copy in normal mode; promised no-copy sites reject in opt mode
   with a useful ownership explanation.
7. Active RFC-0083 loans block the in-place path and cannot cause use-after-free
   or mutation-through-view.
8. Documentation publishes complexity claims only for measured combinations of
   operation, ownership proof, and mode.

## Alternatives

- **Accept copying as permanent.** Semantically valid, but makes repeated
  extraction unsuitable for general-purpose collection and parser workloads.
- **Add `pop_cap`, `dict_remove_cap`, and peers.** Direct, but repeats the
  method-specific path RFC-0051 permits only as a measured bridge.
- **Put every container behind one extraction opcode.** Uniform spelling is not
  a uniform mechanism if the opcode contains a relocated type census.
- **Require uniqueness in normal mode.** Gives predictable performance but
  violates normal mode's copy-correct fallback contract.
- **Bundle extraction into RFC-0087.** Couples a coherent semantic release cut
  to an unproven optimization representation and delays the 0.1 model.

## Drawbacks

- The prototype may show that only the ownership leaf operation is general;
  structural algorithms will still need type-specific implementations.
- Keeping existing paths during transition temporarily increases compiler and
  runtime complexity.
- Normal-mode complexity remains ownership-sensitive even when values are
  semantically identical.
- Useful opt-mode errors depend on facts and diagnostics not yet implemented.

## Prior art

[Perceus](../external-refs/perceus-2021/) and Lean's reuse analysis motivate
reuse under precise ownership. Hylo's mutable value semantics motivate keeping
the optimization unobservable. Swift's consuming and borrowing work informs
move-out safety, while Rust collection APIs illustrate the structural variety
that one ownership primitive must support without pretending every container is
the same.
