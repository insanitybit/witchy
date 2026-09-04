---
rfc: 0165
title: "SafeWIR as the canonical optimizing IR; WIR as a private backend form"
status: proposed
created: 2026-09-03
tracking: impl/safewir-production-ready and successors
---

# RFC-0165: SafeWIR as the canonical optimizing IR

## Summary

SafeWIR becomes the single representation on which the compiler optimizes.
It intrinsically carries the memory semantics of every operation: typed
values, object identity, layouts, places, extents, ownership, effects, and
runtime checks. A small independent checker establishes well-formedness and
memory safety over SafeWIR directly, without inferring anything from
instruction shapes. Optimizations transform SafeWIR into SafeWIR and carry no
authority; each pass is assured externally to a stated level, and the checker
runs again after the last pass. The existing WIR becomes a private, ephemeral,
non-optimizing Wasm serialization form produced mechanically from validated
SafeWIR and encoded immediately. The current adapter that reconstructs
semantics from raw WIR is migration scaffolding and is deleted at the end.

This RFC supersedes the *integration strategy* of RFC-0162 while keeping its
memory model, its judgment vocabulary, and its kernel templates. It strengthens
the exact-byte admission boundary with semantic correspondence and aggregate
artifact binding. It does not weaken any RFC-0162 acceptance row.

## Motivation

The production branch has established a fail-closed validation and admission
boundary worth retaining, but it has not established the complete claim this
RFC requires. Raw WIR is still constructed and optimized before the adapter,
the standard `admit_validated_wir_module(source_bundle, validated)` path still
accepts a supplied `SourceBundleDigest`, synthesizes its AuthorityWIR digest,
and does not carry three independently derived witnesses. A3 therefore fails
on the current branch and that API must be replaced, not wrapped. Exact-byte
matching does not by itself prove encoder correctness. RFC-0165 keeps the useful boundary while replacing the
inference-heavy representation that feeds it.

What is wrong is what feeds the boundary. WIR was designed as a Wasm-shaped
target before any validator existed. It has raw loads from pointer
expressions, i32 arithmetic on addresses, and `WirTy::Bool` used as the
carrier type for object pointers. SafeWIR was retrofitted *around* it: a
15,000-line adapter infers which i32 is an object, which sequence is
object-valued, which arithmetic is a size, and which local holds a buffer,
then hands a reconstructed candidate to a 15,000-line validator. Together with
structure projection, reaching definitions, and the call catalog, the trusted
surface is on the order of 60,000 lines.

The consequences are measurable and were measured this week:

- Every heuristic in the adapter is a place inference can be wrong in either
  direction. Wrong toward rejection wastes effort: a `Bool` carrier where a
  `Str` was needed, a size spelled as i32 multiply instead of i64, two guards
  needed where one disjunction was written, a length-header place given the
  stored value instead of the extent. Wrong toward acceptance is the
  vulnerability.
- Contracts for callable memory effects live in a name-keyed allowlist in
  `codegen/assembly.rs` (`if name == "list_at"`, `if name == "dict_find"`,
  …). Names select implementations; here they grant memory authority. Six
  entries were added this week because it was the only mechanism.
- Coverage does not converge. After a month of multi-agent effort 108 of 177
  example files admit. Every new language feature adds lowering shapes the
  adapter must learn.
- Two genuine security regressions shipped in fast iteration: the guard
  against post-encoding byte rewriting was re-gated on a performance marker,
  and extent promotion stopped recording on the authenticated object, losing
  the count identity extent proofs depend on. Both survived because the
  `witchy-wir` test binary did not compile with the native feature, so the
  adapter suite was silently unexercised.

A trusted core of this size and shape cannot support the intended assurance
claim, that a flaw costs an attacker on the order of $100,000 to find. That
bar is a statement about how much trusted code exists and how it is
established, not about architecture diagrams. The fix is to make the IR carry
the semantics so there is nothing to infer.

## Design

### The pipeline

```text
CheckedModule                       typeck output; unchanged
    ↓ canonical construction
LoweredSafeWir<Candidate>           private origin typestate; typed semantic SSA/CFG
    ↓ initial check + route binding
InitialSafeWir<Valid> + CheckedLoweringWitness
    ↓ pass P₁ … Pₙ                  each: Valid → Candidate → check → Valid
SafeWir<Valid>
    ↓ final check                   the same checker; production-mandatory
ValidatedSafeWir                    private; the only encoder authority
    ↓ one-way mechanical lowering
WasmIr                              private; no optimization; no constructors outside the backend
    ↓ immediate encoding
EncodedModule + wasmparser structural validation
    ↓ semantic correspondence + exact-byte aggregate admission
AdmittedArtifact / receipt-bound launch
```

Authority flows in one direction. `SafeWir<Valid>` and `ValidatedSafeWir` have
no public constructors; only the checker mints them. `WasmIr` has no public
constructors at all and no production path accepts it as input. Nothing
between `ValidatedSafeWir` and `EncodedModule` may mutate the program; a
mutation invalidates admission by construction because admission binds the
encoded bytes to the validated module's digest.

The typestate is the same Rust type at every stage. There is one IR. The
parameter says what may be done with it: a `Candidate` may be inspected and
transformed; a `Valid` may enter a pass; a `ValidatedSafeWir` may be lowered.

### Conditional safety claim and trusted boundary

The claim established by this RFC is conditional:

> Given a `CheckedModule` satisfying the documented frontend assumptions, a
> SafeWIR module accepted by the checker, runtime and host implementations
> satisfying their authenticated contracts, and a conforming Wasm engine, the
> admitted artifact performs no out-of-extent linear-memory access, use after
> release, invalid reclamation, ownership duplication, forged capability use,
> or out-of-contract host-buffer access.

The exact-byte part of this claim depends on the independent SafeWIR-to-WasmIR
correspondence matcher and decoded-byte matcher. A digest establishes identity
and prevents substitution; it does not establish that lowering or encoding
preserved SafeWIR semantics.

This claim does not establish source type soundness, host implementation
correctness, Wasm-engine correctness, or observational equivalence of an
optimization. It applies only to the exact receipt-bound bytes. The complete
property list and assumptions are normative inputs to the checker and backend
tests; an omitted property cannot be inferred from the phrase "memory safe."

The conditional SafeWIR-to-admitted-Wasm trusted computing base includes the SafeWIR grammar and
checker, SafeWIR-to-WasmIR lowering, encoder and kernel templates, final-byte
parser and matcher, candidate identity and receipt binding, runtime
allocation/GC/layout/ABI implementation, import dispatcher, and host import
implementations. During migration it also includes the raw-WIR adapter and
structure projection wherever production still reaches them.

Frontend lowering and optimizers are outside the memory-safety TCB only to the
extent that the final checker independently re-establishes every safety
property they can affect. They remain in the semantic-correctness TCB unless a
before/after refinement checker establishes the transformation. Tests,
fuzzing, or an assurance-record entry increase confidence but do not remove
code from either TCB.

This conditional backend theorem takes canonical `CheckedModule` as an input
assumption, so parser and typechecker correctness are outside that theorem. An
end-to-end source-to-Wasm memory-safety claim additionally includes parsing,
name/trait/monomorphization resolution, type checking, and CheckedModule
serialization in its TCB. The SafeWIR checker independently authenticates every
layout ID against the catalog, rechecks operand and result types at every
operation, rederives callable effects and ownership transitions from bodies,
validates all projections and extents, and matches imports and capability roles
against the ABI. It does not independently prove that the frontend assigned a
source expression its language-specified type or selected the intended
overload; those remain explicit F1 assumptions until separate type-soundness
work closes them.

The theorem has these explicit assumptions:

- **F1 Checked input.** `CheckedModule` supplies canonical value/callable types,
  lifetime and access relations, layouts, imports, and capability kinds. Source
  type soundness is a separate theorem; RFC-0165 assumes these declarations
  satisfy the language specification and authenticates their identity. It also
  supplies the canonical manifest of every RFC-0090 proper-tail obligation.
- **R1 Runtime.** The allocator returns disjoint extents of the requested size,
  memory growth preserves live contents, RC/GC primitives implement their
  catalogued state machines, and layout/ABI catalog versions match the module.
- **H1 Host.** Imports authenticate capabilities and obey their declared buffer,
  retention, reentrancy, effect, and failure contracts.
- **W1 Engine.** The Wasm engine implements the selected Wasm specification and
  target memory configuration, including linear-memory bounds and GC typing.
- **C1 Cryptography.** Candidate and receipt hashes are collision-resistant for
  their domain-separated encodings.
- **L1 Launch.** Only bytes whose complete digest and context match the admitted
  receipt can be cached, published, instantiated, or resumed.

Under those assumptions, the checker and admitted backend enforce:

- **M1 Bounds.** Every linear-memory read, write, copy, and host buffer range is
  contained in one live allocation generation and one permitted projection.
- **M2 Initialization and type.** Reads and typed destruction touch only
  initialized values of the layout-declared realm and type; byte copying never
  duplicates references or authority.
- **M3 Lifetime.** No operation uses a consumed, reclaimed, stale-generation,
  or outlived borrowed value.
- **M4 Ownership.** Unique authority is affine; shared handles balance; loans
  obey access and lifetime rules; destruction and reclamation occur once.
- **M5 Arithmetic.** Size, index, stride, and address conversion cannot become
  negative, truncate, or wrap into an authorized range.
- **M6 Calls.** Direct, indirect, closure, `var`, suspended, and host calls obey
  exact argument, result, write-back, root, authority, effect, and failure
  contracts on every edge.
- **M7 Realm separation.** Linear references, GC references, capabilities, and
  raw bytes cannot be substituted or marshalled without an explicit checked
  operation.
- **M8 Representation invariants.** Collection length never exceeds backing
  capacity, Strings remain valid UTF-8, and layout/header state matches the
  authenticated catalog.
- **M9 Artifact identity.** WasmIR corresponds to the validated candidate and
  the exact launched bytes are those admitted for the same complete context.

Wrong results, changed termination, changed allowed traps, or changed allowed
host effects are semantic-correctness failures even when M1-M9 still hold.

### Three representations, three roles

**SafeWIR contains truth.** A fact belongs in SafeWIR when erasing it changes
the program's meaning or makes its safety policy ambiguous.

**The analysis database contains derived knowledge.** A fact belongs there
when it can be recomputed from SafeWIR and erasing it affects optimization
quality, never meaning. It is keyed to an immutable IR revision and cannot
apply to a later revision unless recomputed or its dependencies are provably
unchanged. A pass never asserts that it preserved an analysis.

**WasmIR contains nothing requiring inference.** It is the existing `WirModule`
family, stripped of every optimization pass and every public constructor, and
retained only so the encoder does not have to be rewritten.

### SafeWIR: types

Value types are the language's own, not carriers:

```text
Scalar     Int | Float | Bool | Unit | V128
Ref<L>     a live object of layout L        (linear memory; carries identity)
Gc<S>      a managed reference of struct S  (opaque; no memory authority)
Cap<K>     a capability of kind K           (externref; never linear memory)
Fn<Sig>    a callable value
```

There is no `i32` in SafeWIR. `Bool` is a boolean. A pointer is a `Ref<L>`.
This single change removes the largest class of adapter inference.

`Int` retains Witchy's specified signed 64-bit arithmetic. Memory operations
use distinct non-negative `Size`, `Count`, `Index`, and `Offset` domains. A
conversion into a target memory width is explicit and checked after evaluating
the mathematical size or address: negatives, overflow, truncation, and
`base + offset + length` wrap are rejected. Division, shifts, narrowing, and
trap ordering retain the source semantics rather than inheriting whatever
Wasm carrier happens to be selected.

Every `Ref<L>` names an object:

```text
ObjectId    { root: DefinitionId, generation: GenerationId }
Layout      canonical LayoutId from the runtime layout catalog
Extent      Exact(Size) | AtLeast(Size) | Counted { owner, header, count, capacity, stride } | Unknown
Provenance  Fresh | Aliases(ObjectId) | Loaded(place) | StaticData(segment) | MayAlias({ObjectId})
OwnershipV1 Unique(OwnerToken) | Shared(SharedHandle) | Consumed
OwnershipV2 OwnershipV1 | ReadLoan(LoanToken) | WriteLoan(LoanToken)
```

These are exactly the validator's current nouns. They move from being
reconstructed to being stated.

A `Counted` extent names the exact owning `ObjectId`, catalogued header
projection, SSA count definition loaded or written through that projection,
backing capacity definition, and element stride. `UpdateCount` creates a new
count definition only after proving `0 <= count <= capacity`; it invalidates
facts tied to the old count while retaining storage generation. A header from
another object or generation never authenticates the extent.

`DefinitionId` versions a value; `GenerationId` versions a backing-storage
lifetime. An in-place mutation creates a new SSA value but retains the same
generation. Allocation, deallocation followed by reuse, or reallocation to
different storage mints a new generation. A growth operation with an
in-place and an allocate-copy arm states both arms explicitly and joins their
complete states rather than calling both outcomes a new generation.

Mutable collections use one correlated SSA state:

```text
CollectionStateCore<L> {
    value: DefinitionId,
    logical_length: DefinitionId,
    storage: ObjectId,
    allocation_extent: Size,
    provenance: Provenance,
}

CollectionStateV1<L> {
    core: CollectionStateCore<L>,
    ownership: OwnershipV1,
}

CollectionStateV2<L> {
    core: CollectionStateCore<L>,
    ownership: OwnershipV2,
    retained_roots: LoanSet,
}
```

Source indexing is always checked against `logical_length`. Allocation extent
exists to prove that the representation can hold all live elements; it never
authorizes source access. Branches and loop headers join the entire correlated
state and discard facts not true on every incoming alternative. Pointer
identity and allocation-growth strategy remain unobservable source details.

### Strings and bytes

`String` is a flat, valid UTF-8 byte sequence. Its stored `byte_length` is the
value returned by `string.length`. `char_count`, `chars`, `char_at`, search
positions, `substring`, and borrowed string slicing use Unicode scalar-value
coordinates (`ScalarIndex`), not byte offsets or grapheme clusters. Scalar
counts and boundary maps are revision-bound derived analyses; changing a byte
body invalidates them.

`substring(s, start, end)` clamps each endpoint into the scalar range and
returns the half-open scalar interval, or the empty string when the clamped
start is not below the clamped end. `chars` produces one valid single-scalar
`String` per element. Invalid scalar inputs to `string.from_code` produce
U+FFFD. Canonically equivalent but differently encoded strings remain distinct;
this RFC does not add Unicode normalization.

`Bytes` is an arbitrary octet sequence with byte length, indices, and slices.
String-to-Bytes preserves the UTF-8 encoding. Bytes-to-String is either
`Bytes.to_string`, which is explicitly lossy and replaces malformed sequences
with U+FFFD, or `decode_utf8`, which is strict and returns `InvalidUtf8`; no
implicit conversion exists.

A `String` may be constructed only from an authenticated valid-UTF-8 source,
scalar encoding, or concatenation/copy of valid ranges whose endpoints are
scalar boundaries. Raw byte operations cannot manufacture a `String`.
Malformed bytes presented to a host contract as `String` fail before the host
effect and are never silently normalized. Default and opt modes preserve all
String and Bytes observables; they may differ only in unobservable storage.

SafeWIR does not collapse the source APIs into one generic string index. Each
operation carries its specified failure policy (`Trap`, `Option`, or `Clamp`)
and coordinate domain (`ByteIndex` or `ScalarIndex`). Lowering obtains those
policies from the checked callable identity, and optimizers preserve them.

### SafeWIR: places

A place is the only way to address memory. It is built by projection from an
object, never by arithmetic:

```text
Place<T> {
    object:     Ref<L>,
    projection: Projection,   // field(i) | element(index) | slice(start, len) | header(k)
}
```

A projection is well-formed only if it is a projection the layout `L` defines.
`element(index)` on a counted layout carries the index as a `DefinitionId`;
the checker relates it to the object's extent. There is no `Place` from
`ptr + 4`, because there is no `ptr`.

Every layout recursively classifies its fields and elements as scalars,
linear-memory references, GC references, capabilities, or nested aggregates.
It also specifies initialization and typed replacement/disposition behavior.
A `Place` is only an addressable typed location. The consuming operation
states whether it reads a copyable scalar, borrows, clones, initializes,
replaces, or moves the projected value; projection alone never transfers or
duplicates authority. Generic `Load<T>` is restricted to recursively
bitwise-copyable, pointer-free `T`. Pointer-bearing values use an explicit
borrow, clone, or move operation.

### SafeWIR v1: operations

The v1 kernel is deliberately small. Every v1 memory-bearing operation is one of:

```text
Alloc        { layout, count?, class }            → Ref<L>  Fresh Unique, kernel storage registered
Reclaim      { object }                                       requires sole authority (below)
Load<T>      { place: Place<T> }                  → T
Initialize<T>{ place: Place<T>, value: T, transfer }           initializes an uninitialized projection
Replace<T>   { place: Place<T>, value: T, transfer }           disposes the old value exactly once
UpdateCount  { collection, new_count }                         checked logical-length transition
ByteCopy     { dst: Place<[u8]>, src: Place<[u8]>, len, overlap }
CloneValue<T>{ value: T }                                      recursively retains shared components
MoveValue<T> { value: T }                                      consumes the source
CloneRange<T>{ dst, src, count }                               typed element clones with rollback
MoveRange<T> { dst, src, count }                               transfers elements and clears source
Project      { object, projection }                → Place<T>
Share        { object }                           → Ref<L>   Unique → Shared, refcount intent
Retain / Release { object }                                   Shared only
Guard        { predicate }                                    a runtime check; traps if false
Call         { callee, args, dests }                          against a CallableContractV1
CallBorrow   { source, callee, parameter }                    synchronous non-escaping read view
TailCall     { site, callee, args }                           mandatory proper-tail transfer
VarCall      { callee, captured_places, args }                 atomic normal-success write-back
HostCall     { import, args, dests }                          against an authenticated ImportContract
MakeClosure  { code, environment, captures, contract: CallableContractV1 }
Suspend      { frame: SuspensionFrameV1 }
Resume       { frame, token }
CancelFrame  { frame, token }
Trap
```

`ByteCopy` is permitted only for layout ranges proved pointer-free. Its
overlap mode is explicit (`memcpy`-like disjoint or `memmove`-like overlap).
It cannot copy a `Ref`, `Gc`, `Cap`, or an aggregate containing one. Typed
clone, move, and replacement recursively follow the layout: unique and
write-borrowed components are affine, shared components retain, and replacing
a live destination releases or disposes its previous value exactly once.

Allocation establishes every projection as `Uninitialized` unless its layout
declares a valid zero/default state. `Initialize` is valid only for an
uninitialized projection and performs no disposition. `Replace` is valid only
for an initialized projection and disposes the previous typed value exactly
once. Initialization state is tracked per projection, meets conservatively at
joins, and causes reclamation to dispose only initialized components. An
`Extent::Unknown` object is opaque: it can be moved, retained, released, or
passed according to a contract, but cannot be projected, loaded, replaced, or
byte-copied until an exact or counted extent is established.

`AtLeast(n)` is established only by a checked allocation, an authenticated
import result, or a validated containing-layout projection. It permits only
projections wholly inside the first `n` bytes. Establishing a counted extent
from a header requires `AtLeast(header_size)`, checked header loads,
`count <= capacity`, non-wrapping size arithmetic, and a separately
authenticated storage extent proving `header_size + capacity * stride` fits.
Header contents alone never authenticate their own allocation extent. This is
the non-circular entry rule for strings and counted collections.

Initialization has a finite structural form. Fixed layouts carry an
`InitMask<ProjectionId>`. Counted collections carry
`InitializedPrefix(count_definition)`: exactly elements `0..count` are live and
the remaining capacity is uninitialized. A slice inherits a proven initialized
interval from its parent. `Initialize` can extend a prefix only at its current
end; shrinking disposes removed elements before publishing the smaller count.
Growing publishes a larger logical count only after all added elements are
initialized. A CFG join accepts equal initialization shapes or an explicit
tagged sum; otherwise predecessors normalize with branch-local initialize or
dispose operations. It never guesses `MaybeInitialized`.

The kernel additionally has `MoveRange<T>` and `CloneRange<T>`.
`MoveRange<T>` transfers initialized typed elements, including owned pointer
slots, to a disjoint destination and clears the source initialization state
before its storage destructor can run. `CloneRange<T>` performs the
layout-defined clone or retain for every element and rolls back initialized
destination elements on failure. Shared-list copy-on-write uses `CloneRange`;
unique-list growth may use `MoveRange`. Memcopying an owned pointer slot without
the matching move-state update or clone/retain is invalid SafeWIR, even if a
legacy helper happens to do so. Both operations have dedicated `RuleId` and
backend-correspondence rows; `ByteCopy` remains pointer-free.

`ByteCopy` requires an initialized source interval. Copy into uninitialized
pointer-free storage initializes the exact destination interval; copy over an
initialized interval uses replacement semantics. Typed destruction follows the
mask or prefix and therefore neither leaks nor double-disposes branch-specific
values.

Plus the scalar and control forms (arithmetic with stated width and wrap or
trap behavior, comparison, `Branch`, `Loop`, `Return`, `Unreachable`) that
carry no memory authority. Checked narrowing is an explicit operation with an
explicit trap, never an implicit conversion.

Arithmetic that feeds a size or an index is ordinary scalar arithmetic. It does
not need to be spelled in a particular shape; the checker evaluates its range
from the analysis it performs, not from syntax.

The semantic conversions `ToIndex(Int)` and `ToSize(Int)` reject negative or
unrepresentable values before any projection or allocation effect. Projection
addressing then evaluates base, layout offset, scaled index, and access width in
mathematical unsigned space and rejects target-width overflow. Dynamic element
access preserves source evaluation order and checks
`0 <= index < logical_length`; allocation bounds are checked separately. The
backend correspondence table fixes the exact unsigned Wasm comparisons and
trap ordering.

### The checker

The checker is the trusted core, exposed through two non-interchangeable entry
points: `check_initial(LoweredSafeWir<Candidate>)` produces
`InitialSafeWir<Valid>` plus `CheckedLoweringWitness`, while
`check_transform(TransformedSafeWir<Candidate>)` produces
`OptimizedSafeWir<Valid>` plus a safety verdict for the refinement framework.
Either returns a precise rejection. The shared rule engine performs its own
dominance, reaching-definition, and range analysis over the candidate; it
trusts nothing carried in from an earlier stage. Its judgments, per operation:

Every normative judgment has a stable `RuleId`. The implementation, focused
tests, mutation tests, correspondence row, and diagnostics all name that same
ID. Adding, deleting, or weakening a rule is therefore an explicit schema
change rather than an untracked validator edit.

| Operation | Judgment |
|---|---|
| `Load` / `Replace` | object live; projection belongs to layout; projected range within current extent (statically, or dominated by a `Guard` that implies it); place initialized for reads; ownership permits the access and typed transfer; replacement disposes the prior value exactly once |
| `Initialize` | projection is in-bounds and uninitialized; the typed transfer is legal; projection becomes initialized |
| `UpdateCount` | collection state and header belong to the same object/generation; new logical length is nonnegative and no greater than authenticated capacity; old count facts are invalidated |
| `Alloc` | size within the kernel class domain; result is the sole token for fresh storage |
| `Reclaim` | storage class is reclaimable; the sole v1 unique affine owner token is live; no shared handle or physical alias remains usable; no later use is dominated; generation advances |
| `ByteCopy` / `CloneValue` / `MoveValue` | layout recursively permits the operation; byte ranges are pointer-free; overlap and initialization rules hold; affine components are never duplicated |
| `CloneRange` / `MoveRange` | every element follows its layout's typed clone or affine move rule; failure rollback and source/destination initialization transitions are exact |
| `Project` | source and projection are live and layout-valid; result is a non-escaping place tied to the operation that consumes it |
| `MoveValue` | every affine source component is live and the mover holds the sole `OwnerToken`; every physical alias through a place, projection, heap/global/frame slot, call or host escape, or returned value is proven ended or structurally unusable; otherwise reject; consume and uninitialize the source exactly once |
| `Share` / `Retain` / `Release` | linear shared-handle discipline; final release atomically produces one checker-private destruction ticket |
| `Guard` | predicate is a scalar boolean; the true edge refines facts on its operands |
| `Call` / `VarCall` | argument ownership meets `CallableContractV1`; results and captured-value ownership match it; `VarCall` commits all value/ownership outputs atomically on normal return and none on trap |
| `CallBorrow` | operand exists only on this `Call`, `VarCall`, or `TailCall` edge; every possible callee parameter is body-derived `ReadOnlyNonEscaping`; no write, move, share, store, capture, return, host retention/reentry, suspension, or post-call use of the view is possible; a `VarCall` source overlaps no captured write-back place; caller ownership is restored unchanged or carried by the tail keepalive |
| `TailCall` | operation is in checked tail position; callable arguments, results, write-backs, effects, and divergence match the enclosing return contract; no frame obligation remains after transfer |
| `HostCall` | import is in the authenticated ABI; buffer arguments are in-contract places; capability values have the required static kind and are dynamically authenticated before effects |
| `MakeClosure` | code identity and environment layout are known; every v1 capture moves, clones, or shares according to its type; captured values and capabilities equal the body-derived `CallableContractV1` |
| `Suspend` / `Resume` / `CancelFrame` | `SuspensionFrameV1` contains every live value, ownership state, reserved place, capability, and staged transaction; the unique token permits exactly one resume or cancel transition |
| join / loop header | facts on each incoming edge meet; an object slot changed on any edge has a declared join; facts that do not hold on every edge are dropped |

### Versioned ownership milestones

The first production schema is deliberately smaller than the full loan algebra
specified below. SafeWIR v1 has only `Unique`, `Shared`, and `Consumed`.
Projection produces a non-escaping `Place`; it does not create a reference or
loan token. Cross-call values are moved, cloned, or shared according to their
contract, and normal-mode mutation of shared value-semantic collections uses
copy-on-write. The v1 `RuleId` registry rejects `OpenReadLoan`,
`OpenWriteLoan`, `Reborrow`, `EndLoan`, `LoanPhi`, `AccessPhi`, borrowed callable
results, and any lifetime-bearing operation as unsupported.

V1 nevertheless has one deliberately non-first-class access form for the
language's normal-mode `let` parameter convention. `CallBorrow` is an operand
of one synchronous `Call`, `VarCall`, or `TailCall`, never an SSA value. A body-derived
`ReadOnlyNonEscaping` parameter contract proves that the callee only reads the
view or forwards it to another such call and cannot write, move, share, store,
capture, return, suspend with, or expose it to a retaining/reentrant host call.
Every indirect target must satisfy the same contract. During the call the
caller cannot use the source; on normal return its exact `Unique` or `Shared`
ownership state is restored without retain, release, or copy. Trap is terminal
for the instance. On `VarCall`, every borrowed source must be root/projection
disjoint from every captured write-back place; all views end before the atomic
write-back commits. On `TailCall`, an unforgeable affine keepalive slot moves
with the tail transfer, keeps each borrowed source live without exposing owner
authority to the callee, and is forwarded or disposed exactly once by the
tail-call state machine. Because the view has one syntactic call extent and cannot
enter a block parameter, local, aggregate, or phi, this rule requires no
regions, reborrows, `LoanSet`, or `AccessPhi`. Any wider or escaping borrow is
v2. This preserves the RFC-0051 in-place path for ordinary `let` calls.

The loan, reborrow, escrow, region, `LoanPhi`, and `AccessPhi` sections define
SafeWIR v2. They become production-capable only under a schema-version change
with their own acceptance evidence. They are intended for opt-mode source
lifetimes and checked in-place/no-copy transformations. A v1 cutover is allowed
only when the frozen baseline has no `v2-required` entry; otherwise the first
production cutover implements the required v2 subset. A normal-mode caller may call an opt-mode module only
through an exported contract containing no lifetime or borrowed result. The v1
vertical slice therefore owns its mutable list uniquely and shares or clones
immutable strings; reclamation requires the unique token and no loan analysis.

SafeWIR v2 extends, rather than mutates, the v1 grammar and serialization with:

```text
ProjectLoan  { object, projection, mode, region } → Ref<L'> + LoanToken
OpenReadLoan { place, region }                    → Ref<L> + LoanToken
OpenWriteLoan{ place, region }                    → Ref<L> + LoanToken
Reborrow     { parent, projection, mode, region } → Ref<L'> + LoanToken
EndLoan      { token }
LoanPhi / AccessPhi
```

Each v2 operation has a distinct schema discriminant and `RuleId`; a v1 parser
rejects it rather than interpreting it as a v1 operation. The v2 judgments are
source/root liveness, parent relation, access mode, region containment,
predecessor correlation, and restoration according to the structural loan
algebra below.

The kernel additionally includes `TailCall`, a mandatory v1 control terminator
rather than an optimization. Tail position is identified during
checked-source-to-SafeWIR construction and cannot be erased into an ordinary
`Call` followed by `Return`. The checker verifies the tail callable contract,
result and write-back equivalence, and absence of live frame obligations. Every
backend has a correspondence row that lowers direct and indirect `TailCall`
through its required proper-tail mechanism or a semantics-equivalent
trampoline. The RFC-0090 300,000-transition probes are a cutover gate in both
modes and backends. An optimization that restructures tail position still
requires per-instance refinement validation.

Canonical `CheckedModule` serialization contains a sorted
`ProperTailSiteManifest`, but the initial checker does not trust that supplied
list. Its independent tail-site enumerator walks the canonical typed AST/CFG
and derives the manifest again from RFC-0090 tail-position rules. `TailSiteId`
is a deterministic function of canonical callable identity and canonical source
operation path, never a frontend-selected nonce. The derived map rejects a
duplicate ID, duplicate source operation, or two IDs for one semantic site.

Each entry records the enclosing callable, checked source operation,
direct/indirect dispatch class, argument/result/write-back contract, observable
effect order, and constant-stack obligation. Every initial `TailCall` carries
its `TailSiteId`. The initial checker requires equality between supplied and
independently derived manifests, then a bijection with initial SafeWIR: every
obligation appears exactly once, every source-origin tail transfer names one
obligation, and its contract and evaluation order match. A missing, duplicate,
or unknown site, or an ordinary `Call` followed by `Return` for any independently
derived site, is a compiler error. `CheckedSourceWitness` and
`CheckedLoweringWitness` commit the independently derived digest.

The tail-site digest is a persistent module invariant. The final checker repeats
the bijection against final SafeWIR, and every `RefinementReceipt` binds equal
before/after tail-obligation-set digests. An optimizer may restructure the
implementation only while preserving each site's semantic `TailCall` or a
correspondence-defined constant-stack tail terminator carrying the same ID.
Optimizer-created tail transfers use a distinct origin and require refinement
evidence; they cannot satisfy an initial manifest entry.

Reclamation is structural, not an inferred whole-program alias census.
Allocation mints one affine unique-owner token. Moving consumes and recreates
that token at the destination; sharing converts it into checked shared
ownership. In v1, reclaim consumes the sole unique token; a shared value is
released through its checked shared state machine. In v2, borrowing additionally
creates structural loan/root tokens carried through aggregates, calls, returns,
storage, closures, and control-flow joins, and reclaim requires every loan to be
discharged. If the checker cannot establish the applicable versioned state, it
retains the storage. No count of
entries in `kernel_storage`, ownership label, or incomplete escape analysis may
authorize destructive reuse.

Shared ownership is a linear state machine. `Share` consumes the unique token
and creates the first shared handle. `CloneValue` creates a new shared handle
and increments the checked count. `Release` consumes exactly one handle. A
non-final release performs no destruction; the atomic final release produces
an internal `DestroyTicket<ObjectId, GenerationId>`. Only that ticket or a
live unloaned unique token can authorize typed destruction and storage
reclamation. Destruction walks initialized components in canonical layout
order, consumes the ticket, advances the generation, and leaves no reusable
authority. The ticket is checker-private and cannot be constructed by lowering
or an optimizer.

Every derived `Ref` carries a root access token, not only references stored in
borrowed aggregates:

| Access state | Read/project | Initialize/replace | Move/share | Reclaim |
|---|---|---|---|---|
| `Unique` with no loans | yes | yes | yes | yes |
| `Shared` | yes | no | clone/release only | final-release ticket only |
| `ReadLoan` | yes/read reborrow | no | no | no |
| `WriteLoan` | yes | yes within loan | no | no |
| `Consumed` | no | no | no | no |

Reborrows cannot outlive their parent. A read loan may be returned or stored
only with its structural root relation. A write loan cannot escape its declared
exclusive region or cross suspension. Ending a loan is an explicit CFG event;
joins retain every possibly live loan and weaken access rather than choosing a
more permissive predecessor.

The structural representation is:

```text
RootAccess {
    base: Unique(OwnerToken) | Shared(Set<SharedHandle>) | Destroyable(DestroyTicket) | Consumed | Conditional(AccessPhi),
    read_loans: Set<LoanId>,
    write_loan: Option<LoanId>,
}
LoanToken {
    id: LoanId,
    parent: Option<LoanId>,
    root: ObjectId,
    mode: Read | Write,
    region: RegionId,
}
LoanSet = sorted map LoanId -> { token, shell_projections }
```

`OpenReadLoan` on a unique root places the owner token in checker-controlled
escrow and adds a read ID; additional sibling read loans are permitted. On a
shared root it adds a read ID plus an internal shared root hold. `OpenWriteLoan`
requires a unique root with no active loan, escrows its owner token, and installs
the sole write ID. `Reborrow` creates a child whose region is contained by its
parent and whose access is no stronger.

Moving a borrowed value moves its `LoanSet`; cloning a read-borrowed shell
clones only the shell and creates a fresh sibling read-loan token against the
same root. `EndLoan`
requires every child ended, consumes the token, removes its ID, and releases
its root hold. Ending the last unique-root read loan or the write loan restores
the escrowed owner token and full unique access. Ending one sibling read loan
does not restore mutation while another remains. Thus authority is restored,
not recreated.

Canonical equality compares all root, token, region, parent, and shell fields.
At a CFG join, identical `RootAccess`/`LoanSet` states join directly.
Differences require an explicit predecessor-tagged `LoanPhi`; owner tokens,
escrow state, loan IDs, roots, and permissions remain correlated inside each
alternative and are never flattened into an unsound cross-product. A unique
and shared alternative must still be normalized by explicit `Share` when code
after the join requires one shared operation.

Operations on `LoanPhi` are checked independently for every alternative and
are admitted only when all alternatives permit them. `EndLoan(LoanPhi)` ends
the selected arm's exact token and yields an `AccessPhi` containing the matching
restored or still-restricted root state. Reclamation through `AccessPhi` is
allowed only when every arm supplies a valid unique owner or final-destruction
ticket for its selected storage. A write loan may join only with the same live
write ID; otherwise it ends on every predecessor. Consumed/live states require
an explicit tagged sum. Loop fixpoints grow a finite set of tagged alternatives
and reject rather than widen away token/escrow correlation.

`AccessPhi` is keyed by the same predecessor selector as its SSA value phi and
contains one complete root/escrow/loan alternative per edge. It is compile-time
state, not a forgeable runtime tag. Every operation is checked pointwise on all
alternatives and produces a correspondingly correlated value/access phi. Phi
elimination preserves the selector and cannot combine an owner token from one
arm with storage from another. When an operation has arm-specific effects,
lowering places those effects on the matching predecessor edge before merging.

### Atomic `var` calls and borrowed aggregates

`var` is represented as a transaction over captured caller places. Places and
their projection coordinates are evaluated once in source order. A normal
return produces the ordinary result plus all next values, ownership states,
and root sets; all write-backs commit atomically before the ordinary result is
usable. A trap or uncaught host failure produces no outputs and commits
nothing. Direct-storage access is an optimization of this operation, not a
different semantic form.

Captured places are evaluated once from left to right and converted to exact
root/projection identities before the call begins. Two write-back places that
are equal, overlap as parent/child or slices, or may dynamically alias are a
compile error; duplicate appearances are not ordered writes. Read-only
arguments that overlap a reserved write-back place observe the pre-call
snapshot and cannot escape it. During the call the reserved place cannot be
read, moved, loaned, or passed to another task through any other path. Commit
is simultaneous over disjoint places, so argument order cannot change
disposition or ownership. If disjointness cannot be established, lowering
fails closed rather than selecting a commit order.

A borrowed aggregate carries an ordered affine `LoanSet` of stable owner roots
and projection-to-shell relations. The set moves with aggregate construction,
storage, calls, returns, and joins. Normal-mode source need not name a lifetime:
when it calls an opt API, the compiler tracks the anonymous bounded loan.
Crossing a relation-erasing boundary materializes an owned value or rejects;
it never silently erases the roots.

`MakeClosure` is the only way to form `Fn<Sig>`. It binds a checked code
identity, exact environment layout, typed captures, retained-root set,
captured capabilities, and immutable body-derived contract. Trait witnesses,
existential adapters, and indirect target sets carry the same sealed contract.
An indirect call is admitted only when every possible target has the exact
canonical access, result, write-back, root, and effect envelope.

Line count is not the target. The target is that every judgment above is a
rule about explicit SafeWIR operands, not a recognition of an instruction
shape. Ten thousand straightforward, independently specified, fuzzed lines are
acceptable; one heuristic that infers an object from a `Bool` is not.

### Runtime checks are the default

A dynamic access is written with a logical-length `Guard`:

```text
Guard(index < length_of(xs))
Load(Place { xs, element(index), Read })
```

The backend always emits the trap. A pass may remove a `Guard` only when it
can show the predicate holds on every path reaching it; that removal is an
optimization with the assurance obligations below, and a failed proof leaves
the guard in place. This lets the compiler be correct and usable before
sophisticated range analysis exists, and makes bounds-check elision an
ordinary, separately assured pass rather than something the validator must
prove for every program.

The backing allocation separately satisfies
`logical_length <= allocation_capacity`. That invariant protects the runtime
representation but does not make elements between length and capacity
source-accessible. If a future source API exposes capacity, it exposes only the
specified lower-bound guarantees; default and opt implementations may choose
different growth factors.

### Capabilities

A capability is a `Cap<K>` and is physically an externref (the existing
`AuthorityRef` path). It is never a linear-memory pointer or an integer. No
`HostCall` accepts an integer where a capability is expected. This is defense
in depth: a SafeWIR defect that corrupts linear memory still cannot
manufacture host authority, because authority is not representable as bytes.

The legacy `WirTy::Capability` carrier is deleted. Any production import that
today receives a capability as an i32 is a forgery route and is closed before
anything else in this RFC.

Static `Cap<K>` typing is necessary but not sufficient. Before any host effect,
the import implementation authenticates the runtime value against the exact
authority identity and operation, rejecting null, wrong-kind, stale,
revoked/inactive, or scope/rights-mismatched values. Target scope is checked at
the action boundary. Authentication failure has the specified trap/error
behavior and performs no host effect. A `Cap<K>` is never stored in linear
memory or copied as bytes; crossing representation realms requires a
catalogued typed marshal operation.

`ImportContract` also states buffer lifetime and pinning, read/write aliasing,
whether the host may retain a value, its effects, and its failure mode.
The authenticated catalog entry is bound to an audited host implementation
manifest containing the implementation artifact digest, ABI/catalog versions,
enabled feature set, and audit identity. Instance creation verifies that exact
manifest before registering an import. A matching import name or contract
identity without the audited implementation binding grants no authority.
Canonical SafeWIR v1 rejects reentry into the same compiled instance or any
callback sharing its linear-memory, ownership, or capability realm. A checked
worker invocation such as `vm.par_map` or `vm.serve` is permitted when it
creates or selects a distinct instance, transfers only contract-authorized
owned/shared values and capabilities through typed marshalling, and cannot
access the caller's places or frame. A synchronous import cannot retain a guest place. Async
behavior uses an owned typed handle plus explicit completion; it never extends
a stack loan implicitly. Wasm traps are terminal for the instance. Recoverable host failures
are explicit result values, so a host cannot publish partial `var` write-back
and then report a recoverable failure.

`Gc<S>` values use typed GC fields and arrays, never `Place` or linear-memory
provenance. Formation, field/array access, and stores are checked against the
exact GC layout; stores emit the target-required barrier. Live SSA values,
closure environments, and suspension frames are GC roots. In v2, a live write
loan may not cross suspension; read loans cross only as explicit pinned frame roots.
Capabilities captured in a resumed frame are authenticated again at each host
effect boundary.

```text
SuspensionFrameV1 {
    state: Running | Suspended(ResumeToken) | Completed | Cancelled,
    continuation: ContinuationId { function, block, operation },
    semantic_site: SuspensionSiteId, // stable identity from CheckedModule
    module_revision_digest,
    compilation_context_digest,
    initialized: InitMask<FrameSlot>,
    locals: [TypedValue],
    reserved_places: [CapturedPlace],
    staged_var_outputs: [StagedWriteBack { place, original_state, next_state }],
    capabilities: [CapabilitySlot],
}

SuspensionFrameV2 {
    base: SuspensionFrameV1,
    retained_roots: LoanSet,
}
```

A suspension frame has the affine states `Running`, `Suspended(token)`,
`Completed`, and `Cancelled`. `Suspend` consumes the running state and moves
all live values, roots, staged `VarCall` outputs, and reserved caller places
into the frame. `Resume` consumes the unique token and restores `Running` once.
Completion atomically commits staged write-backs; trap or cancellation disposes
staged initialized outputs and releases roots without committing. Reserved
caller places cannot be read, moved, or passed to another task while the frame
is suspended. Dropping a frame is exactly cancellation, never an implicit
successful return.

`ContinuationId` names an operation boundary in the exact validated module
revision. The checker proves that the frame layout is the live-in schema for
that continuation and that its normal, trap, and cancel successors are present.
`Resume` requires matching module-revision and compilation-context digests and
branches only to that continuation; a frame cannot resume into another
function, revision, target, or policy. The correspondence receipt binds each
continuation ID, frame layout, resume dispatch edge, and cancel destructor to
its exact WasmIR ranges.

`SuspensionSiteId` is the source-semantic observable identity; the complete
`ContinuationId` is revision-local and remains in the frame for exact lowering
and receipts. A pass that changes control flow supplies a total mapping from
every affected old continuation ID to a new continuation ID carrying the same
semantic site and an equivalent frame schema. Its `RefinementReceipt` binds
that mapping to the before and after revisions. If the independent checker
cannot establish the mapping, the pass may not rewrite across the suspension
point.

`AccessPhi` is keyed by the same predecessor selector as its SSA value phi and
contains one complete root, escrow, and loan alternative for every incoming
edge. It is compile-time checker state, not a forgeable runtime tag. Every
operation is checked pointwise over all alternatives and produces a
correspondingly correlated value/access phi. Phi elimination must preserve the
selector and cannot combine an owner token from one arm with storage from
another. Arm-specific effects are placed on the matching predecessor edge
before the merge. Missing alternatives, mismatched selectors, or a join that
would weaken exclusive access are rejected.

### Callable contracts are derived, never declared by name

```text
CallableContractV1 {
    parameters: [ParameterEntryV1]  // owned/shared or ReadOnlyNonEscaping CallBorrow
    results:    [ResultFormV1]      // owned, fresh, shared, or static
    var_outputs:[WriteBackFormV1]   // correlated alternatives of value and ownership per place
    effects:    [ParameterEffects]  // Read | Write | Retain | Release | Reclaim per parameter
    captured_values: [CaptureContractV1]
    authorities: [AuthorityUse]     // capability slot/kind plus permitted host operations
    host_effects: [HostEffect]
    control: { may_suspend, failure_mode }
}

CallableContractV2 {
    base: CallableContractV1,
    loan_requirements: [LoanRequirement],
    borrowed_results: [BorrowedResultForm],
    var_output_roots: [WriteBackLoanSet],
    captured_roots: [RootContract],
}
```

In v2, a borrowed result uses a first-class multi-root contract. It names canonical
input/lifetime slots and output shell projections, not source lifetime spelling
or runtime addresses. Direct calls, closures, trait witnesses, adapters, and
each possible indirect target preserve this relation exactly. `Unknown` never
stands in for a borrow relation.

`AuthorityUse` names the capability parameter or sealed environment slot, its
kind, attenuation/scope relation, and exact authenticated import operations it
may authorize. A body-derived contract includes every reachable host effect,
including effects reachable through captured callables. Closure formation
proves that its environment supplies exactly those roots and capability slots;
a caller possessing the function value does not acquire any additional
authority. An adapter or indirect target with a wider authority/effect set is
not type-compatible.

For a function with a body, the contract is **derived by the checker from the
body** and compared against the declared signature; a declaration that is
weaker than the body is rejected, and callers see only the checked contract.
Recursive groups are solved as a fixpoint over the strongly connected
component. For an import with no body, the contract comes from the
authenticated host ABI catalog keyed by import identity. There is no third
source. The name-keyed allowlist in `codegen/assembly.rs` is deleted.

The recursive contract domain is a finite powerset of atoms extracted from the
SCC's checked signatures, bodies, layouts, captures, and authenticated imports:

```text
ParameterAccess(parameter, Read | Write | Consume)
ResultOrigin(result, Fresh(layout) | Static(segment) | Input(parameter, projection) | Capture(slot, projection))
RootRelation(result_projection, input_or_capture_slot, root_projection, Read | Write)
VarOutputOrigin(alternative, var_parameter, SameInput | Fresh(layout) | Static(segment) | Input(parameter, projection) | Capture(slot, projection))
VarOutputOwnership(alternative, var_parameter, Unique | Shared | Consumed)
VarOutputRoot(alternative, var_parameter, output_projection, input_or_capture_slot, root_projection, Read | Write)
AuthorityUse(capability_slot, kind, scope_relation, import_operation)
Effect(parameter_or_capture, Read | Write | Retain | Release | Reclaim)
Control(MayTrap | MayDiverge | MaySuspend | FailureMode)
```

The order is subset, the join is set union, and the seed is the empty set.
Calls inside the SCC read the preceding iteration's summaries. Each body walk
adds atoms monotonically. For universe `U`, the powerset lattice has height
`|U| + 1`; a deterministic worklist must reach no-growth after at most `|U|`
successful growth steps or reject an implementation that violates the declared
bound. The least fixed point is the derived contract and must refine the
checked declaration under the variance rules for requirements, results, roots,
authority, and effects. Conflicting result origins remain an explicit finite
union. An atom outside the finite universe, an unrepresentable relation, a
nonconvergent implementation, or an internal `Unknown`/top value rejects the
entire SCC. `Unknown` may be an analysis sentinel but is never published to a
caller or accepted by final validation.

For each `var` parameter, the fixed point must contain one exhaustive finite
set of `AlternativeId` rows. A row contains its origin, exactly one compatible
ownership poststate, and every correlated output root relation. `AlternativeId`
is local to the normal return edge and is preserved through union/fixpoint
iteration. These rows reconstruct the ordered `WriteBackAlternative` list in
the `WriteBackForm` consumed by `VarCall`; fields from different rows can never
be combined. Missing ownership/root output, an
unrepresentable alternative, or disagreement between normal-return edges
rejects the SCC. Trap and divergence edges contribute no write-back atoms;
recursive normal returns still publish all outputs atomically.

A non-singleton `WriteBackForm` has a sealed `AlternativeTag` result lane.
Every normal return emits exactly one tag and exactly the result and write-back
atoms belonging to that alternative. `VarCall` switches on the tag and commits
only that row, as one atomic operation; a trap or divergent edge emits no tag
and commits nothing. The tag is compiler-internal, cannot be observed or
constructed by source code, and is covered by backend correspondence. An
optimizer may erase it only after proving the form singleton and obtaining a
refinement receipt for that concrete invocation. Thus a caller cannot combine
the ownership result from one recursive alternative with the value or root
relation from another.

Contract conformance uses the following variance rules, including while
solving a recursive SCC. Calling convention, `var` slot identities, result
arity, and value types are invariant. An implementation's parameter
requirements must be a subset of the declared requirements. Its possible
result and `VarOutput` alternatives must be a subset of the declared
alternatives. `Unique` is a stronger result guarantee than `Shared` or merely
owned; a borrowed result conforms only to the declaration naming the same root
and loan mode; `Consumed` is exact. Retained roots, capability uses, memory and
host effects, failures, traps, and suspension outcomes must each be subsets of
what the declaration allows. No actual root, authority, or effect may be
omitted. Indirect-call target sets and separately compiled ABI adapters use one
exact canonical declared envelope; a body may be more precise only according
to the subset and result-refinement rules above.

### Optimization framework

A pass is a function from `InitialSafeWir<Valid>` or `OptimizedSafeWir<Valid>`
and a revision-keyed `AnalysisDb` to a `TransformedSafeWir<Candidate>`. The
framework, not the pass, then runs the checker
and, where required, a refinement check:

```text
Safe(after)                         the checker
Refines(after, before)              per pass class, below
```

If either the safety checker or the required independent per-transformation
refinement checker fails, the framework keeps `before`. Subject to those
checkers' stated soundness assumptions, a failed optimization costs performance,
never safety or correctness. An assurance record, focused test, differential
test, or fuzz campaign alone cannot authorize a transformation.

Every pass carries an assurance record and the production pipeline enables a
pass only if its record meets policy for its class:

| Pass class | Minimum to enable in production |
|---|---|
| local algebraic rewrite | mechanically checked rule instantiation for the actual rewrite; differential tests are additional evidence |
| constant folding | executable reference semantics used as a per-instance relational checker; generator fuzzing is additional evidence |
| guard (bounds-check) elision | checked dominating-predicate implication plus preserved evaluation/trap order for the actual removal |
| allocation elision / reuse / reclaim motion | checked ownership transition plus per-instance observable-effect refinement; generated programs are additional evidence |
| CFG restructuring and inlining | per-instance relational control-flow validation including call, return, divergence, trap, and effect equivalence |

Assurance grows over time (focused tests -> property-based generation ->
pipeline fuzzing over random pass sequences -> translation validation -> formal
proof for the smallest critical rewrites). Rechecking every candidate composes
the checker's memory-safety invariant. It does not prove semantic preservation:
tests and fuzzing are bug-finding evidence, not the premise of an inductive
proof. A production pass's required refinement obligation is discharged by a
checked rewrite rule, pass-specific translation validation, or an equivalent
executable relational checker. The final checker remains mandatory because it
speaks about the actual artifact rather than the optimizer implementation.

Every accepted mechanism mints the same private interface:

```text
RefinementReceipt {
    pass: PassId,
    checker_and_rule_version,
    before_revision_digest,
    after_revision_digest,
    observation_model_version,
    before_tail_obligation_set_digest,
    after_tail_obligation_set_digest,
    discharged_obligations,
}
```

Canonical lowering and optimizer output have distinct private typestates. The
initial checker accepts only `LoweredSafeWir<Candidate>` created by the one
route-manifested lowering entrypoint; it rejects `TransformedSafeWir<Candidate>`.
After independently checking SafeWIR safety and the checked-module/context
identity, it mints:

```text
CheckedLoweringWitness {
    canonical_checked_module_id,
    initial_safewir_revision_digest,
    proper_tail_site_manifest_digest,
    CompilationContext,
    lowering_schema_and_route_digest,
}
```

This witness authenticates origin and initial safety, not source-lowering
semantic correctness; canonical lowering remains in the semantic-correctness
TCB until it has its own correspondence proof. Its constructor and the origin
typestate are private to the initial checker boundary.

The optimizer framework folds the exact ordered receipt chain into a canonical
body, and an independent chain validator later seals it for admission:

```text
OptimizationHistoryBody {
    checked_lowering_revision_digest,
    ordered: [RefinementReceipt],
    final_safewir_revision_digest,
    optimizer_policy_and_route_digest,
}

OptimizationHistoryDigest = H("witchy-optimization-history-v1", canonical_body)

OptimizationHistoryWitness {
    CandidateId,
    CompilationContext,
    body: OptimizationHistoryBody,
}
```

Receipt `i` must end at the revision where receipt `i + 1` begins; the first
begins at the revision authenticated by the consumed `CheckedLoweringWitness`
and the last ends at the
final SafeWIR revision. The zero-pass case is an explicit empty witness whose
initial and final digests are equal. After `CandidateId` commits the body
digest, the independent chain validator checks every link, the optimizer policy
and route, the initial checked-lowering digest, and equality between the body's
final digest and canonical final SafeWIR before minting the opaque witness.
Final encoder authority requires it. Safety revalidation of an arbitrary final
candidate cannot synthesize or replace semantic-refinement history.

The observation model includes returned values, normal write-backs, allowed
trap condition and order, divergence, host/capability effects and order,
suspension/cancellation behavior, and every source-observable allocation or
resource guarantee. It excludes pointer identity and physical capacity where
the language declares them unobservable. The pass framework accepts only a
receipt minted by a registered independent checker for the exact before/after
digests and complete obligation set. Unsupported or inconclusive equivalence
keeps the original revision; a boolean callback or self-asserted pass result is
not an equivalent checker.

The versioned observation trace is a canonical sequence of
`Return(value, write_backs)`, `Trap(code, effect_index)`,
`HostEffect(import, public_arguments, guest_buffers_before_after,
external_state_observation, retained_handles, instance_or_worker_identity,
result_or_failure)`, `Suspend(semantic_site, public_frame_state)`,
`Cancel(semantic_site)`, and `ResourceObservation(contract_id, metric,
scope, required_bound_or_relation, observed_delta_or_peak)` events. Resource
metrics include stack depth, peak memory, allocation/copy counts, and explicit
no-copy/in-place identity relations. Divergence is modeled
as trace-prefix refinement rather than a terminal event. Values use language
equality; capabilities use kind/scope identity without exposing host object
identity; physical allocation events are absent unless an explicit opt resource
contract materializes them as `ResourceObservation`. A relational checker states
which trace-refinement relation it proves and the receipt binds that relation's
schema version.

Analyses (`range_of`, `may_alias`, `is_unique`, `extent_of`, `effects_of`)
are queries over an immutable revision. A stronger analysis answers the same
query more precisely and every optimizer benefits without changing SafeWIR.
An analysis result may be materialized into the IR only as a `Guard` whose
predicate the checker re-establishes; materialization grants nothing. V1 has no
implicit or undefined `Assert` operation.

The legacy `inline_direct_calls` and `inline_recursive_calls` passes were not
dead: the former shipped in the default release optimization set and the latter
ran unconditionally. Both duplicated effectful call arguments by substituting
argument expressions at every parameter use. They are sealed from production
and retained only under `cfg(test)` for regression coverage until RFC-0165
deletes them or replaces them with assured SafeWIR passes. Neither may regain a
production caller.

### Normal and opt performance contract

There is one language semantics. Normal mode prioritizes fast iteration and
may preserve correctness by retaining runtime guards, allocating, or inserting
one explicit copy/re-own repair when ownership precision is insufficient.
`mode opt` is the lever for near-native performance: it exposes no-copy,
uniqueness, lifetime, layout, and bounded-resource contracts, and rejects a
missing proof rather than silently violating the requested cost model.

Both modes preserve values, traps, write-back, ordering, text coordinates,
capability effects, and observable alias behavior. They may differ in
unobservable allocation identity, physical capacity, growth factor, and
whether storage is reused. Analyses are deliberately extensible so stronger
range, alias, ownership, loop, and profile information can enable additional
SafeWIR rewrites without expanding the semantic kernel. A failed proof keeps
the runtime check or original operation in normal mode and is the documented
diagnostic in opt mode when an explicit performance contract requires it.

"Observable alias behavior" means ordinary value semantics: assigning or
passing a collection cannot cause mutation through one value to alter another
value, while an explicit live borrow observes only the owner state and access
permitted by its lifetime contract. Allocation addresses, backing identity,
refcounts, and physical capacity are not equality or reflection observables.

Opt obligations come from the checked callable/access type. `unique` forbids a
hidden re-own copy at that boundary; `local unique` additionally forbids escape;
an explicit bounded/in-place contract also requires sufficient authenticated
capacity and forbids allocation for the covered operation. Plain uniqueness
does not forbid allocation when growth requires it. A rejection identifies the
exact obligation and value/place path that lost it. Normal mode may repair only
contracts the language marks repairable and records the repair in deterministic
resource counters.

For the initial language-semantics revision, normal mode may repair only: a
missing `Unique` proof for an ordinary owned value when no loan, must-consume
obligation, or local-unique escape exists; mutation of a shared collection by
one semantics-preserving copy-on-write; and representation re-ownership that
preserves the exact source result and is not under an explicit no-copy,
in-place, or bounded-resource contract. It may not repair use after move, a
live-loan conflict, overlapping `var` roots, a lifetime escape, loss of a
must-consume value, escape of local uniqueness, a capability or realm mismatch,
or violation of an explicit resource promise. This classification is versioned
by `language_semantics_revision`; adding a repair is a language-semantics
change, not an optimizer policy change.

Migration cannot declare success by preserving safety while losing the current
in-place/no-copy performance class. The de-optimized copy-correct configuration
is the semantic oracle; deterministic allocation/copy/reuse counters and
matched workload budgets guard the opt performance contract.

Acceptance creates and then freezes `perf/safewir-rfc0165.toml`. Each entry
contains the source and input digests, supported mode/optimization matrix,
metric, sample protocol, pre-migration baseline commit and toolchain, and
numeric budget. The initial manifest includes every maintained workload named
by `spec/performance.md` plus every workload whose lowering or optimizer is
changed by this RFC. It uses the repository's kernel `bench_ns` metric for
runtime work and separately records wall time, peak memory, compilation time,
and deterministic ownership/allocation counters.

The freeze records counters per workload and input, never only in aggregate.
For every RFC-0051-sensitive workload it includes successful in-place
mutations, copy-on-write repairs, retain/release traffic, allocations,
reallocations, storage reuse, and peak live bytes. A16 compares each counter
with that workload's frozen value or stricter checked budget. `CallBorrow`
coverage must preserve the frozen in-place count and may not hide a regression
behind aggregate runtime variance.

Unless a workload already has a stricter checked-in budget, acceptance requires:

- identical semantic output and allowed traps against the interpreter and
  de-optimized oracle, with no new OOM;
- zero repair copies where the checked opt contract forbids them, and no
  regression in allocation, re-own-copy, or reuse counters for other opt cases;
- per-workload opt median `bench_ns` and peak memory no worse than 1.05 times
  baseline, with suite geometric-mean time no worse than 1.02 times baseline;
- per-workload normal-mode median `bench_ns` and peak memory no worse than 1.10
  times baseline, with no workload exceeding its checked-in resource ceiling;
- normal-mode median compile/check latency no worse than 1.10 times baseline
  and p95 no worse than 1.15 times baseline; and
- matched quiet runs on the same declared host, with an inconclusive noisy run
  rerun rather than counted as passing.

A baseline or threshold change requires a reviewed manifest delta containing
raw before/after samples, correctness evidence, and the reason the prior budget
is obsolete. Safety work cannot silently rebaseline a performance regression.
These are migration non-regression gates; Witchy's broader faster-than-managed
runtimes and near-native targets remain separately measured product goals, not
claims established merely by adopting SafeWIR.

### WasmIR and the encoder

Lowering `ValidatedSafeWir -> WasmIr` is a total, one-way, structurally
recursive function with no analysis, layout inference, contract lookup, or
optimization. "Mechanical" is a design restriction, not a correctness proof.
Every SafeWIR operation has a specified correspondence preserving values,
control flow, traps, memory and ownership effects, imports, and capability
arguments. The actual lowering result requires a checked correspondence or
translation-validation artifact; until that exists, the lowering remains in
the TCB. If a step cannot be specified locally, the operation it needs belongs
in SafeWIR instead. `WasmIr` is encoded in the same call; it is never stored,
cached, serialized, or exposed. The
`raw-encoder-test-api` family of features remains dev-only and is verified
absent from every production build by the existing feature-gating test, which
becomes a required gate.

The normative lowering correspondence is operation-classed:

| SafeWIR class | Required WasmIR correspondence |
|---|---|
| scalar/control | same width, signedness, evaluation order, branch target, result arity, divergence, and trap condition |
| place access | catalogued base/offset/stride, checked target-width address, exact load/store width and alignment |
| guard/conversion | equivalent unsigned predicate and trap ordering before the protected effect |
| typed initialization/replacement | exact component encoding plus one matching retain/release/disposition sequence |
| allocation/destruction | matching kernel class, mathematical size, generation transition, and canonical initialized-field destruction order |
| call/closure | exact callable envelope, environment layout, result/write-back lanes, root companions, and target set |
| host call/capability | exact authenticated import identity, buffer extent/direction, capability lane, effect, and failure contract |
| GC/suspension | exact GC type, root/barrier behavior, frame layout, and resume edge |

Lowering produces a `CorrespondenceReceipt` independently checked against both
the complete `ValidatedSafeWir` and complete `WasmIr`. The receipt identifies
every source operation and target instruction range and asserts the table row
that relates them. Admission binds this receipt; an uncovered or multiply
claimed target instruction is a compiler error.

Every target instruction is owned exactly once by one of
`SourceOp(OperationId)`, `KernelTemplate(TemplateId, Version)`, or
`StructuralRule(CorrespondenceRuleId)`. Function framing, locals, block
structure, and mandatory traps use reviewed structural rules. Runtime helpers
are either ordinary validated SafeWIR functions or versioned kernel templates
with their own complete correspondence; the encoder cannot inject anonymous
helper instructions. Data, type, import, table, and custom sections are covered
by corresponding catalog entries rather than instruction mappings.

The receipt contains `CandidateId`, the complete `CompilationContext`, canonical
WasmIR digest, ordered per-function mappings, and section mappings. An
independent matcher parses the complete WasmIR,
recomputes the ownership partition and each local table judgment, and mints the
opaque receipt only when no operation, instruction, helper, or section is
missing, duplicated, reordered incompatibly, or mapped under the wrong rule.

SEC-128 closes only when admission consumes three separately derived witnesses
for the same compiler-minted candidate identity: checked-source authority,
final SafeWIR safety authority, and authenticated import/AuthorityWIR
authority. No caller supplies a digest, and no witness is manufactured solely
from another witness. Exact-byte matching derives its expected structure from
an independent specified representation; replaying the production encoder can
detect mutation but cannot prove the encoder's interpretation correct.

The shared identity is:

```text
CandidateId = H(
    "witchy-safewir-candidate-v1",
    canonical_checked_module_id,
    canonical_final_safewir,
    proper_tail_site_manifest_digest,
    checked_lowering_witness_digest,
    optimization_history_digest,
    checked_module_schema_version,
    language_semantics_revision,
    safewir_schema_version,
    layout_catalog_version,
    host_abi_catalog_version,
    runtime_kernel_contract_version,
    gc_contract_version,
    host_capability_contract_version,
    target_memory_configuration,
    lowering_policy_version,
    correspondence_schema_version,
    wasmir_schema_version,
    encoder_schema_version,
    wasm_feature_set,
)
```

Those version/catalog/target fields, in that order, are the canonical
`CompilationContext`. The tuple's encoding uses the same explicit
length-prefix, discriminant, and integer rules as canonical SafeWIR. Every
post-candidate witness and receipt stores both its `CandidateId` and complete
`CompilationContext`; equality is byte-for-byte canonical equality, not a
subset or compatibility comparison. The pre-candidate `CheckedLoweringWitness`
and each `RefinementReceipt` instead bind the complete context and their exact
module revisions; their canonical digests are committed by `CandidateId` and
the ordered optimization history.

The encoded-byte matcher independently parses the exact emitted bytes and
produces both `exact_wasm_bytes_digest` and
`decoded_canonical_wasmir_digest`. It does not receive either digest as a
caller-selected expectation. Admission requires the latter to equal the
`canonical_wasmir_digest` in the correspondence receipt and then mints exactly
one aggregate binding:

```text
ArtifactBinding = H(
    "witchy-admitted-artifact-v1",
    CandidateId,
    CompilationContext,
    checked_lowering_witness_digest,
    canonical_wasmir_digest,
    exact_wasm_bytes_digest,
    optimization_history_digest,
)
```

The admitted artifact owns the immutable byte buffer and this binding. Cache,
publication, deserialization, and launch APIs accept only that aggregate and
recheck the exact byte digest before use; they cannot combine individual
receipts or substitute another WasmIR or byte buffer.

Concrete runtime and host build identity is launch authority, not compilation
identity. `LaunchContext` binds `ArtifactBinding`, the audited host
implementation manifest digest, runtime artifact digest, engine build/config,
and instance policy. Launch verifies that the selected implementation manifest
conforms to the host ABI/catalog versions already committed by `CandidateId`,
then mints the launch receipt. The same admitted artifact may run against a
different audited conforming host build without recompilation; no unaudited or
ABI-mismatched build can acquire launch authority.

Canonical serialization, field order, and hash domain separation are part of
the schema. Each opaque witness and intermediate receipt binds the candidate
identity and context; every artifact consumer binds the aggregate
`ArtifactBinding`. Witnesses from different schemas, catalogs, targets,
policies, WasmIR instances, or byte buffers cannot be combined.

Canonical SafeWIR encoding is a versioned length-prefixed binary form with
explicit enum discriminants and fixed-width little-endian integers. Sequences
preserve semantic order; sets and maps sort by canonical encoded key. Functions
sort by checked identity. Blocks use reverse postorder with stable successor
tie-breaking, and values/operations receive dense IDs from that traversal.
Source spelling, filesystem paths, allocation addresses, randomized hash-map
order, and diagnostic-only names are excluded. Text is valid UTF-8 preceded by
its byte length. The checked-module and catalog encodings obey the same rules
under distinct domain tags.

The three witnesses have non-overlapping derivations:

- `CheckedSourceWitness` is minted directly from canonical checked-module
  types, access/lifetime relations, declared imports, capability kinds, and the
  complete proper-tail-site manifest.
- `FinalSafeWirWitness` is minted by checking the canonical final SafeWIR and
  records its `RuleId` verdicts.
- `AuthorityWitness` is minted by an independent walk that compares every final
  `HostCall` and captured callable authority with the authenticated ABI and the
  checked module's available capability slots. It is not derived from either
  other witness's authority graph.

The operation-class correspondence table above is only the normative schema.
Before any `RuleId` can be implemented or enabled, a versioned correspondence
registry must contain a per-operation row specifying its exact operands,
results, control and memory effects, emitted Wasm instruction sequence, trap
behavior, and source-location mapping. The registry is exhaustive: A15 fails
if a SafeWIR operation lacks a row or backend code emits instructions not
accounted for by that row.

Admission requires the three authority witnesses, the
`OptimizationHistoryWitness`, the correspondence receipt, and the independently
parsed encoded-byte receipt to carry one identical `CandidateId` and context
tuple. It separately recomputes the canonical `CheckedLoweringWitness` digest,
requires equality with `CandidateId.checked_lowering_witness_digest`, and
compares that witness's `CompilationContext` directly. It also requires the
history body digest to equal the digest
committed by that candidate. It additionally requires exact equality between the decoded
WasmIR digest in the byte receipt and the canonical WasmIR digest in the
correspondence receipt before minting `ArtifactBinding`. A field or digest
mismatch is not repairable by recomputing one witness from another.

## Migration

This is a codegen-output rewrite, not a flag day. Unsupported forms fail
closed throughout; there is never a raw fallback.

Every compiled module has exactly one optimizer authority. Before the atomic
production cutover, the frozen adapter route may run legacy WIR optimizations
and the SafeWIR-native route is development-only. A module on that route runs
no SafeWIR optimization. After cutover, production constructs and optimizes
SafeWIR directly; any WIR optimizer or analysis reached afterward is a build
failure. No module traverses both optimizer stacks, and WasmIR only lowers and
encodes.

One checked route manifest enumerates every production compiler entrypoint,
feature configuration, constructor, pass ID, and encoder authority. Structural
source analysis over the fixed crate/file and macro-expansion policy rejects an
unlisted route, mutation, raw constructor, encoder call, or test-only feature.
The checker and optimizer likewise use normative `RuleId` and `PassId`
registries; evidence is keyed to those IDs rather than comments or names.

Production cutover is one merge-queue candidate, not a runtime flag. The
candidate is eligible only when a generated `cutover-ready.json` binds the
current candidate commit and records A1, A3-A13, A15, and A16 green, complete
parity with the frozen adapter-admitted baseline, and no unresolved critical/high review
finding. The candidate itself must make A2 and A14 green by removing production
adapter/raw authority. The integration owner may submit this candidate but
cannot mint readiness; the serialized merge coordinator recomputes the file,
runs the full candidate gate, and is the only mechanism that advances master.
The baseline is a sorted, digest-bound snapshot taken when the adapter freezes:
every source/configuration admitted by that ordinary production route, with its
canonical observation contract. Cutover may not lose any entry. The complete
known-valid source universe remains an explicit post-cutover coverage ledger;
an entry outside the frozen admitted set may remain fail-closed without blocking
cutover, but cannot be removed, reclassified, or counted as a regression-free
success. This is a finite no-regression cutover, not a big-bang requirement.

Every frozen-baseline entry is additionally classified as `v1-direct`,
`v1-materialized`, or `v2-required`. Materialization may replace only an
internal, source-unobservable borrowed lowering with an owned clone or checked
shared retain/release at the same evaluation point; mutation then follows v1
copy-on-write, preserving values, traps, effect order, and deterministic
resource counters. It is forbidden when the checked source exposes a lifetime,
borrowed result, no-copy/in-place promise, or other observable opt contract.
Any such admitted entry is `v2-required`, and cutover waits for v2 support for
that entry. The freeze gate measures this before v1 implementation planning;
staging is not permission to reclassify or drop existing support.

A frozen compatibility case declares its exact input and environment domain.
Finite domains are enumerated; infinite domains name a versioned generator and
coverage obligations and are explicitly empirical compatibility evidence, not
a universal proof. Canonical lowering remains in the semantic-correctness TCB,
as stated above. The cutover claim is therefore limited to the frozen cases and
domains actually exercised; it must not be described as whole-program semantic
equivalence.

A freeze observation contract uses the same versioned observation model as
optimizer refinement: return values and atomic write-backs, allowed trap and
divergence behavior, ordered host/capability effects and failures,
suspension/cancellation events, and source-observable resource guarantees. A
deterministic case stores the canonical trace under digest-bound inputs,
host-event script, clock/random seed, runtime catalog, and target context.
Nondeterminism is represented as an explicit domain of environment event
streams. Adapter, interpreter, and SafeWIR executions are coupled to the same
stream and must produce equal observation traces pointwise. Both directions are
required: neither route may add failures/outcomes or omit an allowed outcome.
A predicate may classify environment values but may not widen the resulting
trace relation. Final output or trap class alone is never parity evidence.

1. **Gate.** The `witchy-wir` and `witchy-lower` native suites compile and run
   as a mandatory red-build gate. The example census
   (`scripts/safewir-census.sh`) consumes a committed sorted manifest of every
   source in `examples/`, `projects/`, bundled `std/`, package fixtures,
   generated test-driver templates, and feature-gated normal/opt/native
   profiles. Each entry is classified as known-valid, intentionally invalid,
   or requiring a named unavailable external fixture. Every known-valid entry
   runs through the ordinary production CLI. Additions, removals, renames,
   profile changes, and exclusions require an explicit manifest delta.
   Raw-entrypoint unreachability is a gate.
2. **Freeze.** No new adapter heuristics. No new name-keyed contracts. Seal the
   two active, unassured legacy inliners from production and retain them only
   for regression coverage until deletion. Close any i32 capability import.
3. **Specify.** This RFC's kernel, types, and judgments become
   `crates/witchy-wir/src/safewir/` as Rust types with private constructors.
   Existing `ObjectIdentity`, `Extent`, `Provenance`, `Ownership`,
   `KernelAllocationClass`, callable, and SafeKernel implementations are reused
   only where they implement the semantics in this RFC. Retrofit assumptions
   are not preserved merely to avoid rewriting them.
4. **Vertical slice.** One program flows SafeWIR-native end to end — construct
   from `CheckedModule`, check, one pass, recheck, lower, encode, admit,
   execute, parity — with the adapter not on the path:

   ```witchy
   fn main(console: Console):
       var out = []
       for n in [1, 2]:
           out.push("x")
       console.print(out.join(" "))
   ```

   It exercises allocation, projection, dynamic extent, a loop join, a call
   with an object result, reclaim, and a host call, and it is the current
   reclaim-class reproducer.

   The slice is accepted only with a companion fixture bundle: a successful
   dynamic indexed load and an out-of-bounds trap; `VarCall` normal and trap
   atomicity; authenticated capability success and refusal; an exact
   body-derived callable contract; the `unreachable_pruning` SafeWIR pass with
   its executable before/after refinement checker; and a stage trace proving the adapter and every WIR
   optimizer unreachable. Run the bundle in normal mode, opt mode, and the
   copy-correct de-optimized oracle.
5. **Migrate by semantic family**, in this order: strings and byte bodies;
   scalar lists; pointer lists and GC aggregates; dictionaries; closures and
   indirect calls; capabilities and host calls; generators and async frames.
   Each family lands with its focused probes and its census delta. Helpers in
   `wir_helpers/` are rewritten to emit SafeWIR operations directly, which
   retires their hand-written contracts.
6. **Move optimizations.** Proper tail calls migrate into mandatory SafeWIR
   construction and backend lowering. The live peephole and unreachable-pruning
   passes are reimplemented as SafeWIR passes with executable per-instance
   refinement checkers or disabled. No optimization remains on WasmIR.
7. **Delete.** At the no-regression production cutover, delete
   `safewir_adapter.rs`, the structure projection over raw
   WIR, the name-keyed contract table, and every `WirModule` constructor
   outside the backend. Rename `WirModule` to `WasmIr`.

Before cutover, the adapter remains the current frozen production route and
receives no new heuristics, contracts, or optimizer semantics; it may also be
used for differential evidence. The atomic cutover candidate deletes the
adapter, raw structure projection, name-keyed contract table, and all production
raw authority. No adapter or mixed production route remains after cutover.

### What carries over

The validator's memory model and judgment vocabulary; the SafeKernel
templates; the exact-byte admission boundary, receipts, and launch path; the
reaching-definition and range machinery (as analyses, no longer TCB); the
census script and probes; the security findings ledger. The sunk cost is the
adapter and the name-keyed contract table.

### Concurrency

Proceeds concurrently: parser and type-system work; frontend features that
lower into frozen SafeWIR operations and ship a probe; new analyses;
runtime hardening; tooling. Serialized under one owner: the SafeWIR kernel
and its judgments; ownership and reclaim rules; contract derivation; the
capability representation; the backend boundary; enabling any optimization
in production. Stopped: new WIR optimizations, new name-keyed authority, new
adapter pattern recognition, any production fallback to raw encoding.

Only the kernel owner changes operation schemas, `RuleId`s, layouts, callable
contract encoding, candidate identity, or correspondence tables. Family tracks
consume a released schema version and submit additive kernel-change requests;
they do not fork semantics locally. The integration owner alone advances the
schema version, updates the shared vertical fixture, and reconciles family
branches. Analysis and lowering extensions may proceed independently when they
consume the frozen schema without expanding authority.

## Acceptance

Implementation rows are production-path mechanical gates; the TCB and
correspondence review row is explicit human security evidence. Registry
entries, source searches, census counts, merge metadata, and fuzz corpus size
are supporting evidence only. Claim-bearing artifacts record the exact SafeWIR
candidate and emitted-byte identities and fail when stale, bypassed, or
produced through a non-production entrypoint. A row is complete only when its
evidence exists on current `master` with a terminal `merged` event.

| # | Criterion | Evidence |
|---|---|---|
| A1 | The native `witchy-wir` and `witchy-lower` suites compile and pass; a compile failure of a security suite is a red build | CI job; `cargo nextest run -p witchy-wir --features native` green |
| A2 | Only the private backend can construct `WasmIr`, only from `ValidatedSafeWir`; no production caller can invoke a raw encoder or test API | checked route manifest plus structural source/call-graph and feature-tree gate |
| A3 | Admission requires all three authority witnesses, the independently minted checked-lowering witness, and the exact ordered optimization-history witness; the zero-pass case is explicit; no public API accepts a caller-supplied source digest | SEC-128 closed; `compile_fail` doctests; transformed-as-lowered, mismatched source/lowering/history/final revision, and zero-pass refusal tests |
| A4 | No `HostCall` capability role accepts a non-`Cap<K>`; runtime kind/scope/revocation authentication precedes effects; each import is bound to its audited host implementation manifest; isolated workers cannot share the caller realm; legacy capability carriers are gone | structural source gate plus wrong-kind, stale, revoked, null, scope-mismatch, implementation-digest, catalog-version, same-instance callback, shared-memory/frame/place, and shared-capability-realm refusals |
| A5 | lowering and transformed candidate typestates, `SafeWir<Valid>`, `ValidatedSafeWir`, `DestroyTicket`, `CheckedLoweringWitness`, `OptimizationHistoryWitness`, and correspondence/admission witnesses have no constructors outside their validators | structural visibility gate and compile-fail fixtures |
| A6 | The canonical v1 operation list is exhaustive; every registered judgment, numeric conversion, `CallBorrow`, typed copy/move/range rule, extent-bootstrap rule, and realm boundary has focused positive, negative, and mutation coverage | `RuleId`-keyed manifest including operation-list completeness, CallBorrow escape/suspend/write/indirect-target/VarCall-overlap/tail-keepalive refusals, pointer-range ownership, and extent-bootstrap refusals, with no missing or orphan evidence |
| A7 | V1 reclaim requires the sole structural affine owner and has no loan operations; missing evidence retains storage; storage-map counts never grant authority | negative tests for duplicate/shared owners and escaped roots; v2 loan-channel tests gate only the v2 schema |
| A8 | V1 callable contracts are body-derived, include atomic `var` outputs, and survive every direct/indirect adapter; the name-keyed table is deleted; exact multi-root borrowed results gate v2 separately | body/ABI conformance, closed target-set witnesses, and contract-erasure refusals |
| A9 | The adapter-free vertical bundle covers dynamic index success/trap, loop join, `VarCall` success/trap, authenticated capability success/refusal, derived calls, one checked pass, and exact admission | normal/opt/de-opt production probes plus a stage trace excluding adapter and WIR optimizers |
| A10 | A committed sorted manifest classifies the full source universe, immutably marks the adapter-admitted freeze baseline and its v1-direct/materialized/v2-required status, declares each exercised input/environment domain, and compares complete observation traces pointwise without claiming universal equivalence | digest-bound replay domains and traces, bidirectional nondeterministic coupling, materialization probes, host buffer/state/effect ordering, stack/peak-memory/no-copy resource observations, and compatibility gate |
| A11 | An independent checked-AST walk proves manifest completeness and uniqueness; initial and final checkers prove a bijection with proper-tail operations; every refinement receipt preserves the tail-set digest; no optimization runs on `WasmIr`; every enabled pass contributes to admission history | omitted/duplicate/aliased-ID/unknown/`Call; Return`/optimizer-erasure refusals, RFC-0090 300,000-transition probes on both backends, route manifest, chain/zero-pass refusals, and per-pass relational artifacts |
| A12 | The production-active legacy inliners remain sealed and are deleted once their regression corpus is transferred to assured SafeWIR replacements or explicit non-replacement evidence | structural syntax-tree gate over the optimizer source manifest plus exactly-once effectful-argument regressions |
| A13 | Pipeline fuzzing compares generated programs and random valid pass sequences with replayable failures | nightly production-path artifact recording generator version, seeds, valid-pass coverage, and candidate IDs; bug-finding evidence only |
| A14 | The adapter, raw structure projection, and every `WirModule` constructor outside the backend are deleted | structural syntax-tree/call-graph gate; `WirModule` renamed |
| A15 | The canonical operation list, conditional safety properties, complete TCB, and SafeWIR-to-WasmIR correspondence are exhaustive; nothing in the final checker infers semantics from instruction shape | structural operation/RuleId/correspondence bijection plus adversarially reviewed specification and backend artifacts |
| A16 | Normal mode, opt mode, and every supported optimization configuration preserve language behavior and meet per-workload frozen performance and in-place-path budgets | `perf/safewir-rfc0165.toml`, RFC-0051 workload counters, interpreter/de-opt parity, raw samples, and enforced per-workload thresholds |

A11 and A16 are joint cutover prerequisites. There is no dated or test-only
optimizer exception: a pass needed to meet A16 must first have its executable
per-instance refinement checker. A pass without one remains disabled, and the
candidate must still meet A16. Failure means the cutover waits rather than
shipping unchecked optimization or silently weakening the performance budget.

When A1-A16 are complete and the checker, backend correspondence, and optimizer
composition have received independent adversarial review, the project may make
only the conditional claim stated above. Coverage evidence proves usability,
not soundness; fuzzing increases confidence but does not turn the conditional
claim into a proof of the entire compiler or frontend.

## Review checklist

RFC-0165 is ready for design review when reviewers can decide each item below
from this document and the referenced language specifications, without
inferring intent from the current adapter:

1. The conditional safety property and every frontend, runtime, host, engine,
   and artifact-identity assumption are complete enough to falsify.
2. Each memory operation has a total `RuleId` judgment over explicit operands,
   including initialization, numeric conversion, trap order, and realm rules.
3. Unique, shared, borrowed, moved, and destroyed states form a structural
   affine state machine; no incomplete alias census can authorize reclamation.
4. `VarCall`, closures, indirect calls, borrowed aggregates, loops, and
   suspension preserve exact values, write-backs, ownership, and multi-root
   loans across every CFG edge.
5. UTF-8 validity, byte versus scalar coordinates, typed copying, GC barriers,
   and host reentrancy/retention are unambiguous.
6. Normal mode retains its copy-correct development behavior, while opt mode
   can exploit stronger analysis and enforce no-copy/performance contracts
   without changing language behavior.
7. `CandidateId`, three independent witnesses, correspondence receipt,
   emitted-byte receipt, aggregate `ArtifactBinding`, and launch receipt bind
   the same schema, catalogs, target, policy, canonical WasmIR instance, and
   exact immutable bytes.
8. The migration permits only one optimizer authority per module, freezes
   semantic drift, and has a mechanically fixed source universe and cutover.
9. Every acceptance row names evidence that exercises the production route;
   tests and fuzzing are not represented as universal proofs.

Review findings that change any of these decisions require an RFC revision and
schema-version impact statement. Implementation may prototype against the
proposal, but acceptance waits for an independent adversarial review with all
critical and high findings resolved or explicitly rejected with rationale.

## Non-goals

This RFC does not claim source-language type soundness; that is a separate
project. It does not require formal verification of the whole compiler; it
requires a small trusted core, external assurance per pass, and an
independent final check. It does not add a native backend; it makes one
possible later, since only the lowering from `ValidatedSafeWir` would change.

## Alternatives

**Finish the retrofit.** Continue migrating helpers and adding adapter
heuristics and contract entries until coverage is complete. Rejected: it does
not converge, it grows the trusted surface with every feature, and its
per-helper contracts are the mechanism by which guessed authority enters.

**Metadata over WIR.** Keep raw WIR and annotate it. Rejected: annotations
detach from the instructions they describe under transformation, must be
re-verified against the arithmetic, and become authority rather than
disposable analysis. This is the retrofit with a different name.

**Proof-carrying IR.** Embed proof objects in SafeWIR that optimizers must
carry. Rejected in favour of external assurance: proof terms can be copied or
forged by a buggy pass, and the checker can re-establish the same judgments
from explicit operands without them.

**Runtime-only safety.** Emit every check and never elide. Retained as the
*default* within this design, but insufficient alone: the helper library and
allocator need static discipline, and the language's performance targets need
elision to be possible.

## Drawbacks

This is a codegen-output rewrite. `codegen/mod.rs` alone is over ten thousand
lines and every lowering path emits raw WIR today. The work is finite and
converges, but it is weeks to months. Before cutover, the development-only
SafeWIR route may reject programs the frozen production adapter admits; the
adapter remains production until SafeWIR reaches exact freeze-baseline parity.
After cutover, remaining source-universe gaps are recorded pre-existing
fail-closed coverage gaps rather than regressions. Assurance per pass is real
ongoing cost; a pass without adequate relational evidence does not ship. The
hard parts are body-derived recursive callable contracts and establishing the
backend correspondence without recreating an inference-heavy second compiler.
Structural loan transport through higher-order and suspended control flow is a
v2 cost, not part of the first production cutover.
