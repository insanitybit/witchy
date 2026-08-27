---
rfc: 0162
title: "SafeWIR schema 1 normative validation judgment"
status: active
created: 2026-08-27
---

# SafeWIR schema 1 normative validation judgment

This is the normative contract draft for SafeWIR schema 1. The schema and rule
catalog are the contract for the implementation tracks; this document by itself
satisfies no RFC-0162 acceptance row. Validator, kernel/encoder, migration,
security, mutation, pipeline, resource, and zero-cost evidence are tracked
independently in the live implementation acceptance ledger.

This document is the independent normative judgment required by RFC-0162. It
specifies what a schema-1 SafeWIR validator may accept; Rust control flow is not
the specification. Validator diagnostics, positive and negative fixtures, and
mutation tests cite the stable rule identifiers below.

Changing an obligation requires a new SafeWIR schema version. Adding an
implementation check that rejects programs outside this judgment is permitted
only as a fail-closed resource limit or diagnosed unsupported form. Weakening a
rule in place is not permitted.

## 1. Domains

- `d` is a `DefinitionId`, assigned to each parameter, local write, expression
  result, synthetic join, and multi-result destination.
- `g` is a `GenerationId`, minted by allocation and destructive reuse.
- `l` is a canonical RFC-0111 `LayoutId` present in the module layout graph.
- `o = (root=d, generation=g)` is object identity.
- `extent` is `Unknown`, an exact mathematical byte expression, or
  `header + count(d) * stride`. `Unknown` never proves an access in bounds.
- `projection` is an ordered path of zero or more descriptor-derived header,
  field, element, and slice steps. Each step is interpreted relative to the
  preceding step; candidate offsets are never authoritative.
- `p` is provenance: `Fresh` for the containing object identity,
  `Aliases(o, projection)`, `MayAlias(O)`, `StaticData(o)`, or `Unknown`.
  Every referenced object includes both root and generation. Empty
  `MayAlias` sets are invalid rather than a stronger no-alias fact.
- `q` is ownership: `Unique`, `Shared`, `BorrowedRead`, `BorrowedWrite`, or
  `Consumed`.
- `r = (o, start, length)` is a byte range. `start` and `length` remain
  mathematical integers until an explicit checked narrowing.
- `object = (identity, layout, extent, provenance)` and
  `place = (object, projection, range, access)` are untrusted candidate
  descriptions of one requested typed access. Constructing either grants no
  authority. The projected layout is derived from the object's canonical
  descriptor and projection path; a candidate does not supply it separately.

The validator state at a program point is:

```text
State = {
    reaching: Local -> nonempty set DefinitionId,
    values: DefinitionId -> ValueFact,
    generations: ObjectIdentity -> Ownership,
    borrows: ObjectIdentity -> set Borrow,
    ranges: DefinitionId -> RangeFact(object, generation, length_source),
}
```

Absence of the whole range fact is `Unknown`; its object, generation, and
length-source fields are never individually optional. Absence of any other fact
is likewise `Unknown`, never `Fresh`, `Unique`, non-wrapping, or in bounds.
Candidate `ObjectRef`, `Place`, `Provenance`, and `Ownership` values are claims
only. The authoritative ownership and borrow states exist solely in the
validator's private maps and can be created or transitioned only by successful
application of the rules below. Facts are compiler-only and erase before
encoding.

`ValueFact` includes RFC-0153's signed mathematical range and non-wrap facts.
The candidate `SizeExpr::Definition(d)` is not a sign proof: SWIR-RNG-001 must
find a nonnegative fact for `d` in the private value map before narrowing.

## 2. Module and layout rules

### SWIR-MOD-001 - checked source entry

There is exactly one production path from checked, linked Witchy source to a
candidate module. Every function and global has a checked WIR type. Test-only
forged candidates are unavailable without test configuration. Missing or
contradictory source type/layout information rejects.

### SWIR-LAY-001 - canonical layout

Every object, projection, callable boundary, retain/release, host buffer, and
semantic memory operation names a `LayoutId` in the canonical layout graph.
Projection layout and byte range are derived from that descriptor. A caller,
callee, constructor, boundary adapter, and selected kernel template must agree
on the same ID. Unknown, fabricated, or conflicting IDs reject.

## 3. Provenance, ownership, and lifetime rules

### SWIR-PROV-001 - destructive provenance

`Release(unique)`, `Move`, and `Reuse` require `Fresh` for the containing exact
object identity. `Aliases`, `MayAlias`, `StaticData`, and `Unknown` never authorize
a destructive operation. They may authorize reads or conservative retention
when all range and layout rules hold.

### SWIR-OWN-001 - unique allocation

`Alloc(l, count)` validates non-wrapping extent arithmetic and mints exactly one
`Unique` token for a fresh `(root, generation)`. A checked
`Fresh(layout, extent)` call result transfers the callee's single allocation
token into a new caller-side result identity; it does not duplicate the token.
These are the only origins of a unique generation. Sharing/retaining
transitions according to the operation contract; it cannot preserve a
simultaneous unique token.

### SWIR-OWN-002 - consume once

`Move`, destructive `Reuse`, and unique `Release` require the sole live unique
token and no live alias or borrow that conflicts with the operation. They mark
the input generation `Consumed`. A consumed generation cannot reach a later
memory, retain, release, move, host-buffer, or reuse operation.

### SWIR-GEN-001 - live generation

Every place, byte range, alias, borrow, and range fact names the exact current
generation. `Reuse` consumes the old generation and mints a distinct new one
even when the numeric address is unchanged. Facts naming an older generation
do not transfer to the new generation.

## 4. Arithmetic and range rules

### SWIR-RNG-001 - non-wrapping size arithmetic

Header-plus-payload, count-times-stride, base-plus-offset, and
start-plus-length are proved in mathematical integers. A `NarrowU32` conversion
to a Wasm address bit pattern is accepted only after proving the mathematical
value is in `0..=u32::MAX`. A signed source index must first be proved
nonnegative; only then may it be interpreted and narrowed as unsigned. This
does not impose an `i32::MAX` address ceiling: Wasm `i32` memory operands use
all 32 bits as an unsigned address. Overflow, an unproved narrowing, or using a
wrapped machine result as proof rejects.

### SWIR-RNG-002 - exact range source

A `ProvenIndex` fact identifies the index definition, object root, generation,
and exact length-source definition. It must dominate the access in the final
control-flow graph. Reassignment of any source, object reuse, a changed length
source, merge without the fact on every edge, replacement of the defining
expression, or any intervening operation that may mutate or escape the object
invalidates it. In particular, a store to any descriptor component on which
the extent depends and a call whose parameter effect includes `Write`,
`Consume`, `Retain`, `Escape`, or `Unknown` kill the fact. `CheckedIndex`
remains valid by retaining its semantic runtime check.

## 5. Control-flow dataflow rules

### SWIR-CTL-001 - conservative join

At a join, a fact is retained only when valid for every reaching definition on
every incoming edge. Distinct roots, generations, layouts, projections, or
length sources do not unify. A unique token survives only when every edge
returns the same compatible unique generation. Otherwise destructive use is
rejected; conservative shared/unknown treatment may continue only when it does
not invent ownership or provenance.

### SWIR-CTL-002 - loop fixpoint

Loop headers start from entry facts and are iterated with a deterministic work
list to a conservative fixpoint. Back edges apply SWIR-CTL-001. Widening may
discard numeric precision but cannot create a bound, provenance, ownership, or
generation fact. Exceeding the configured iteration or fact budget rejects
under SWIR-BUD-001.

## 6. Callable rules

### SWIR-CALL-001 - checked callable contract

Every memory-bearing direct, indirect, closure, recursive, imported primitive,
and host callable has a contract:

```text
parameters = [{ entry, set(Read, Write, Consume, Retain, Escape) } | Unknown]
entry = NoMemory | BorrowedRead | BorrowedWrite | Shared | Unique | Unknown
result = Fresh(layout, parameter-relative extent)
       | Aliases(parameter, constant projection)
       | Shared(parameters)
       | NoMemory
       | Unknown
```

The final body and call graph validate the contract. Recursive components use a
conservative fixpoint. Imported contracts are checked against their kernel or
host definition. Parameter effects form a finite union lattice: join is set
union, the empty set means no memory effect, and `Unknown` is the top set
containing all effects. Missing information starts at `Unknown`, not the empty
set. A parameter may therefore both write and retain. `Unknown` forces the
validator to assume that the callee may read, write, consume, retain, and escape
the argument; it cannot preserve unique authority, destination-forwarding
eligibility, or object-header-derived range facts. An attached optimizer
summary has no authority by itself.

At callee entry, the checked source access signature and contract must agree.
An owned/consuming parameter transfers the caller's sole token into callee
`Unique`; an exclusive-write parameter enters `BorrowedWrite`; a read borrow
enters `BorrowedRead`; a retained/shared parameter enters `Shared`; a scalar
enters `NoMemory`. Missing or contradictory entry information is `Unknown` and
cannot authorize destructive use. `Consume` requires `Unique`; `Write` requires
`Unique` or `BorrowedWrite`; and the final state must satisfy the declared
retain, escape, and consume effects. Entry transfer moves existing authority;
it does not mint a second allocation token.

Contract projections are caller-substitutable: header and field steps are
descriptor ordinals, while element and slice steps use contract constants.
They never contain callee-local `DefinitionId`s. A result whose projection or
extent depends on an unexported callee definition is `Unknown`. Fresh extents
are expressions over constants and caller parameter positions. Validation
substitutes the caller's argument definitions, checks layout and extent, then
mints one new caller result root and generation with one `Unique` token. An
empty `Shared` parameter set rejects rather than meaning "aliases nothing."
An `Aliases(parameter, ...)` result and every member of a `Shared(parameters)`
result require the corresponding parameter effect to include `Escape`; an
out-of-range parameter index rejects.

### SWIR-CALL-002 - unknown result is not fresh

A missing, stale, contradictory, or imprecise result contract is `Unknown`.
`Unknown` may be read, returned, or conservatively retained when other rules
permit; it cannot authorize free-at-overwrite, destination forwarding,
unchecked projection, unique release, move, or destructive reuse.

## 7. Memory and boundary rules

### SWIR-MEM-001 - object range

Every `Load`, `Store`, `Fill`, retain/release header access, and semantic kernel
operation names a live object generation. Its byte range satisfies
`0 <= start`, `0 <= length`, and `start + length <= extent` under
SWIR-RNG-001. A range inside Wasm memory but outside the object rejects.
`ByteRange.object`, `start`, and `length` are deliberate candidate claim/check
pairs: validation compares the identity with the containing place and derives
the range from the descriptor and projection. Redundancy is not independent
authority and must not be optimized into trust.

### SWIR-MEM-002 - projected layout

The canonical descriptor determines projection offset, width, alignment, and
child layout. Header steps name descriptor components, never candidate byte
offsets or lengths. Load/store width is not independent evidence. Fields,
elements, headers, packed values, sums, tuples, records, closures, and
specialized values must use the descriptor-derived range and exact layout.

### SWIR-BULK-001 - bulk ranges and overlap

`Copy` validates complete source and destination ranges independently, their
layouts and ownership effects, and the declared overlap behavior. `Fill`
validates the complete destination range. Zero length does not excuse invalid
object identity or wrapped range arithmetic.

### SWIR-REF-001 - reference/scalar separation

Linear-memory scalar contents cannot become `externref`, abstract or concrete
GC references, capability references, closure environments, or layout IDs.
Reference values remain reference-typed through locals, calls, tables,
structures, arrays, and imports. No scalar-to-reference conversion exists.

### SWIR-HOST-001 - host buffer ABI

Every host-call buffer argument names a live object generation, exact layout,
and in-object byte range matching the checked host ABI contract. The host's
independent policy and resource checks do not discharge this compiler rule.

## 8. Kernel, encoding, and resource rules

### SWIR-KERN-001 - sealed raw memory

Ordinary lowering, helpers, optimizers, and production tests cannot construct
raw loads, stores, copies, fills, heap-global writes, or raw function bodies.
Only the named encoder kernel-template module can expand validated semantic
operations to raw instructions. Privileged operations are opaque to the normal
inliner. Each has an explicit contract and negative test.

### SWIR-ENC-001 - validated encoder authority

The production encoder accepts only a `ValidatedWirModule` whose constructor is
private to successful final validation. Its only post-validation transform is
fixed expansion of semantic operations and fact erasure. Standard Wasm
validation is mandatory. Test-only raw encoders cannot link into production.

### SWIR-BUD-001 - budget exhaustion fails closed

Validation has explicit deterministic recursion, work-list, and fact budgets.
Exhaustion rejects the candidate with the named limit. A retained semantic
candidate may be validated independently from its own entry state; the rejected
candidate is never repaired or encoded. Exhaustion of both candidates fails
compilation.

## 9. Transfer summary

| Operation | Required facts | State transition |
|---|---|---|
| `Alloc` | canonical layout; non-wrapping extent | mint fresh root/generation and one `Unique` token |
| `Project` | live object; canonical child descriptor | derive exact place layout/range; preserve generation |
| `CheckedIndex` | live object; checked signed index and length | derive place after semantic check |
| `ProvenIndex` | SWIR-RNG-002 dominating fact | derive unchecked place for exact object/generation/source |
| `Load` | read access; SWIR-MEM-001/002 | ownership unchanged |
| `Store` | write access; SWIR-MEM-001/002; borrow compatible | ownership unchanged |
| `Copy` / `Fill` | SWIR-BULK-001 | apply declared ownership/copy contract |
| `Retain` | live owning root; canonical RC layout | transition/share according to contract |
| `Release` | live owning root; matching layout | consume unique token or perform checked shared RC operation |
| `Move` | sole unique token; no conflicting alias/borrow | consume input token, transfer unique authority |
| `Reuse` | destructive rules and non-wrapping new extent | consume old generation, mint distinct new generation |
| call | checked callable contract | union parameter effects; kill dependent facts on mutation/escape; substitute a declared alias, or transfer exactly one fresh allocation token into a new caller identity |

## 10. RichWasm correspondence

The basis column refers to RichWasm's syntax and memory model in §2.1, dynamic
heap operations in §3.1, static type system in §4, and erasure/lowering in §6
of [Paraskevopoulou et al., *RichWasm*](https://arxiv.org/abs/2401.08287).
The cited system uses typed locations, size expressions, read/read-write
privileges, linear/unrestricted qualifiers, capabilities, and structured heap
operations. SafeWIR adopts those proof ideas; it is not an implementation of
the RichWasm calculus.

| SafeWIR rule family | RichWasm basis | Named Witchy extension |
|---|---|---|
| SWIR-LAY-001, MEM-001/002 | typed locations, sizes, capabilities | canonical `LayoutId`, specialized descriptors, exact byte ranges |
| SWIR-OWN-001/002 | linear versus unrestricted capabilities | RC transitions, conservative retention, explicit consume/move |
| SWIR-PROV-001 | ownership of typed locations | alias projections and `Unknown` default-deny provenance |
| SWIR-GEN-001 | allocated-location ownership | generation identity across numeric-address reuse |
| SWIR-RNG-001/002 | typed allocation size and access bounds | RFC-0153 dominance facts tied to definitions, generation, and length source |
| SWIR-CTL-001/002 | structured typed control flow | mutable-WIR reaching definitions, joins, widening, and loop fixpoint |
| SWIR-CALL-001/002 | typed functions/modules | checked alias/freshness/consume summaries and recursive components |
| SWIR-REF-001 | separated typed values | Wasm `externref`/GC/capability representation boundary |
| SWIR-HOST-001 | typed module boundary | Witchy host ABI live-range contracts |
| SWIR-KERN-001, ENC-001 | erasure to core Wasm | final-candidate validation, sealed templates, private encode authority |
| SWIR-BUD-001 | no direct correspondence | fail-closed compiler resource policy |

The RichWasm type-safety theorem does not prove Witchy's validator, kernel,
encoder, or Binaryen pipeline. Every item in the extension column remains a
Witchy proof/testing obligation.

## 11. Required rule evidence

Each rule identifier requires:

1. one accepted fixture exercising its positive premise;
2. one forged candidate rejected specifically for that rule;
3. a mutation that deletes, inverts, or weakens the implementation check and is
   killed by the fixture set; and
4. for kernel/encoder rules, exact byte/template and production API coverage.

Coverage of one rule cannot stand in for another. A green workspace test, a
source census, or backend agreement alone is not rule evidence.
