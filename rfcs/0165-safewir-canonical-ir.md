---
rfc: 0165
title: "SafeWIR as Witchy's canonical optimizing IR"
status: proposed
created: 2026-09-03
revised: 2026-09-04
tracking: impl/rfc0165-canonical-v1
---

# RFC-0165: SafeWIR as Witchy's canonical optimizing IR

## Summary

Implementation amendment: [one memory model and checked opt boundaries](0165-memory-mode-boundary.md)
records the 2026-09-04 decisions on normal/opt interoperability, managed read-only
views, source `unique`, and deferred mutable-reference/region interfaces. It
supersedes conflicting staging choices below, not safety or tail-call obligations.

SafeWIR becomes the only representation on which Witchy performs semantic or
memory-affecting optimization. It states language-level types, layouts,
extents, ownership operations, runtime guards, call conventions, host
contracts, and proper-tail control flow directly. The checker validates those
semantics without reconstructing them from Wasm-shaped instruction sequences.

Derived optimization knowledge lives in a separate analysis database keyed to
an immutable SafeWIR revision, analysis version, and program point. Missing
knowledge is conservative: guards remain, reference counts remain, copies
remain, and storage is not reused. More precise knowledge may improve generated
code but cannot be required for baseline memory safety.

Existing WIR becomes a private, non-optimizing Wasm serialization form. This
RFC calls it WasmIR. A checked SafeWIR module lowers through a small reviewed
operation mapping, is encoded immediately, and is bound to the exact bytes that
may execute. Raw WIR is never a production input or fallback after cutover.

Implementation is staged. The first production milestone has no optional
optimization passes. It establishes useful, runtime-checked SafeWIR coverage,
checked reference counting with dynamic uniqueness, an executable SafeWIR
semantics, backend correspondence, and an atomic production cutover. Later
milestones add analyses and enable optimizations only with safety rechecking and
semantic-preservation evidence.

This RFC supersedes RFC-0162's inference-heavy integration strategy. It retains
RFC-0162's memory model, kernel-layout work, fail-closed policy, and exact-byte
artifact boundary.

## Goals

1. Make memory safety straightforward to state and independently check before
   Wasm exists.
2. Preserve the semantic information optimizers need instead of recovering it
   from pointer arithmetic and carrier types.
3. Make a missing analysis cost performance, not correctness or language
   coverage.
4. Permit increasingly precise analyses without expanding the semantic kernel
   for every optimization.
5. Keep the trusted core reviewable and eliminate helper-name and
   instruction-shape authority.
6. Establish the exact optimized SafeWIR module as the only source of
   production Wasm bytes.

## Conditional safety claim

The initial claim is conditional on the checked frontend, runtime, host, and
Wasm engine assumptions listed below. For a module accepted by the SafeWIR
checker, the SafeWIR-to-WasmIR mapping, and exact-byte admission, execution may
not:

- access outside a live object's authorized extent;
- interpret initialized storage using an incompatible layout;
- use an owned reference after it has been moved or dropped;
- duplicate an owned reference without the corresponding checked retain;
- reclaim storage while a counted owner remains;
- wrap, truncate, or sign-confuse an address into an authorized range;
- manufacture host authority from linear-memory bytes or scalar values;
- invoke a host operation outside its authenticated ABI contract; or
- omit a runtime safety check unless the final checker independently proves
  its predicate on every reaching path.

This claim does not establish source-language type soundness or universal
semantic correctness of optimizations. Those are separate obligations.

### Trusted assumptions

- The canonical checked module supplies sound source types, declared layouts,
  parameter conventions, capability kinds, imports, and proper-tail sites.
- The runtime allocator returns disjoint storage of the requested size and its
  reference-count primitives implement the specified state machine.
- Authenticated host implementations obey their buffer, capability,
  reentrancy, and retention contracts.
- Wasm linear memory, references, traps, and control flow implement the
  validated Wasm semantics.

The SafeWIR checker independently checks every use of these declarations. It
does not independently prove the frontend's source typing theorem.

## Pipeline

Before production cutover:

```text
current production compiler                  frozen; no new semantics
checked module -> development SafeWIR route  explicit selection; no fallback
```

After cutover:

```text
CheckedModule
    -> SafeWir<Candidate>
    -> independent safety check
    -> SafeWir<Valid>
    -> zero or more checked SafeWIR passes
    -> final independent safety check
    -> ValidatedSafeWir
    -> reviewed SafeWIR-to-WasmIR mapping
    -> private WasmIR
    -> immediate deterministic encoding
    -> independent Wasm validation and correspondence evidence
    -> exact artifact binding
    -> AdmittedArtifact
```

There is one production route after cutover. Unsupported operations reject;
they never select the frozen compiler per program.

## SafeWIR contains semantics

SafeWIR is SSA and CFG based. Its stable value categories are:

```text
Scalar<Int | Float | Bool | Duration | Unit | V128>
OwnedRef<Layout, AllocationClass>
StaticRef<Layout>
GcRef<Struct>
Cap<Kind>
Fn<Signature>
```

Wasm carrier types are backend details. A source boolean is not a pointer. A
linear-memory object is not an integer. A capability is an opaque host
reference and cannot be synthesized from linear-memory contents.

### Layouts and extents

Every memory-bearing reference has a catalog-authenticated layout and one of:

```text
Exact(bytes)
HeaderPrefix(minimum_bytes)
Counted {
    logical_count: ValueId,
    capacity_count: ValueId,
    stride,
    header_layout,
}
```

There is no general `Extent::Unknown`. `HeaderPrefix` permits only the exact
catalog header fields within `minimum_bytes`. Header fields are initialized by
allocation or an authenticated host result and may be changed only by checked
metadata operations such as `UpdateCount`; general stores cannot target them.
After validating the loaded count, capacity, stride, arithmetic, and catalog
relationship, the checker may establish `Counted` authority.

Logical length controls source indexing. Capacity controls allocation and
growth only. Capacity and growth policy are not required to match between
normal and opt modes.

### Places

A place is a typed projection used directly by a memory operation:

```text
Place<T> {
    root: ReferenceId,
    projection: Field | Header | Element | Slice,
    access: Read | Write,
}
```

Places are not first-class values. They cannot be stored, returned, captured,
placed in block parameters, or retained across an ownership operation on their
root. The checker re-establishes root liveness and extent at every use. This
prevents a projection from becoming a hidden dangling reference.

### Runtime bounds are the default

A dynamic indexed access contains an explicit guard:

```text
Guard(0 <= index && index < logical_length)
Load(ElementPlace(object, index))
```

The baseline backend emits that guard. An optimization may reorganize or remove
guards only when the final checker proves the required predicate from the final
CFG using its own versioned theory. Optimizer analyses never grant checker
authority.

## V1 affine owned-reference model

V1 does not infer a permanent `Unique` or `Shared` state. Each `OwnedRef`
value is one affine ownership token backed by the allocation class's runtime
discipline.

```text
AllocRc       -> one OwnedRef
MoveRef       -> transfers the token and consumes the source value
CloneRef      -> retains and creates one additional token
DropRef       -> consumes one token; recursively destroys at runtime count zero
IsUnique      -> runtime predicate that the current RC allocation has count one
```

Static uniqueness is future derived knowledge. Dynamic `IsUnique` is the
baseline in-place mechanism. A collection mutation uses the unique branch when
the runtime count is one and otherwise performs a typed value-semantic copy.
This preserves normal-mode value semantics while retaining the RFC-0051
reclamation and in-place usability floor without implementing a borrow checker.

Reference counts cannot wrap. `CloneRef` traps or fails before overflow.
`DropRef` cannot underflow. A zero transition produces one runtime-private
destruction ticket and no SafeWIR operation can forge that ticket.

Destruction is layout driven. It drops every initialized owned-reference child
exactly once before reclaiming the shell. Partial initialization is explicit,
joins preserve only initialization facts true on every incoming edge, and a
failed aggregate construction rolls back exactly the children already
initialized. A shell-only free is not a valid implementation of `DropRef` for
a pointer-bearing layout.

Allocation classes have distinct legal operations:

| Class | CloneRef | DropRef | IsUnique | Reclaim |
|---|---|---|---|---|
| reference counted | yes | yes | yes | at zero only |
| static | no-op identity only | no-op | false | never |
| bump | no | region-defined | no | region only |
| scratch | no | scope-defined | no | scope only |

Bump and scratch references cannot escape their checked region or masquerade
as reference-counted objects.

### Typed aggregate movement

Pointer-free bytes may use `ByteCopy`. Pointer-bearing values use:

```text
MoveValue<T> / MoveRange<T>
CloneValue<T> / CloneRange<T>
ReplaceValue<T>
```

Move consumes each source ownership token and clears source initialization.
Clone executes the layout-defined retain recursively. Replace drops the old
initialized value exactly once. Raw byte copying of an owned reference is
invalid SafeWIR.

## Calls without v1 loans

V1 calls use the same owned machine ABI for source `let` and `own`
parameters. The source convention controls what the callee may do; it does not
create a hidden backend borrow.

- A non-tail caller that preserves an owned argument executes `CloneRef`
  before transfer.
- A consuming call transfers the existing token with `MoveRef`.
- The callee must move, store, return, or drop every received token on every
  exit.
- A proper tail call transfers the caller's token. No caller drop or other frame
  obligation follows the edge.
- Returned references are owned tokens. The caller accounts for them locally.
- Indirect calls require an authenticated structural signature, never a helper
  name.

There is no v1 `CallBorrow`. Retain/call/release is the conservative normal
form. Later optimization may remove balanced retains only after proving the
callee cannot retain or expose the reference.

Whole-program result-origin inference is not required for baseline memory
safety. A function body is checked locally against its structural ABI: input
tokens balance, output token types match, `var` write-backs are explicit
results, and capability/host effects fit declarations. More precise body
summaries remain useful derived optimization information.

### Interior and borrowed-looking values

A v1 operation such as `slice` or `as_str` must choose one of two owned
representations:

- materialize a fresh value when copying is source-unobservable; or
- return a fat owned view containing a counted owner token plus a checked range.

Dropping the view drops its owner token. The range is validated when the view is
created and again when used if mutation could change the owner's extent. No
naked interior pointer or lifetime-bearing result exists in v1.

Explicit source lifetimes, escaping borrowed results, and optimizations that
must recover static uniqueness belong to the v2 loan milestone. A normal-mode
caller may call an opt-mode module only through an exported owned ABI with no
borrowed result. An accepted opt no-copy promise must be represented and
enforced; v1 must reject rather than silently honor it with a copy.

## Host operations and capabilities

`Cap<K>` is represented by an opaque host reference. The host validates
dynamic kind, scope, generation or revocation state, and provider identity
before any effect. No integer or linear-memory pointer can satisfy a capability
role.

Host buffer places are valid only for the dynamic extent of one call. A
non-reentrant synchronous import cannot retain them. A same-instance reentrant
import receives an additional cloned owner token for every buffer root, making
runtime uniqueness false in re-entered Witchy code. Isolated-worker APIs may
transfer owned values through checked marshalling but cannot share the caller's
linear memory, places, frames, or unauthenticated capability realm.

Async host operations use owned typed handles or copied buffers. They do not
extend an ephemeral place across suspension.

## Proper tail calls

Proper tail behavior is language semantics under RFC-0090, not an optional
optimization. Checked source supplies a canonical proper-tail-site manifest.
The initial checker independently derives tail obligations and requires a
bijection with SafeWIR `TailCall` terminators. Missing, duplicated, unknown, or
ordinary `Call; Return` encodings reject.

A `TailCall` moves all live owned arguments and leaves no caller ownership,
write-back, or destruction obligation. The backend must implement direct and
indirect tail sites using target tail calls or a semantics-equivalent
bounded-stack dispatcher. Tail legalization is mandatory and has dedicated
correspondence evidence plus the RFC-0090 300,000-transition executable probe.

## V1 operation kernel

The canonical v1 memory-bearing operations are exhaustive:

```text
AllocRc, AllocRegion
CloneRef, MoveRef, DropRef, IsUnique
Project
Load, Initialize, Replace, StoreScalar
ByteCopy, MoveValue, CloneValue, MoveRange, CloneRange
UpdateCount
Guard, CheckedNarrow
Call, VarCall, TailCall, MakeClosure
HostCall
Trap
```

Scalar arithmetic, comparison, SSA binding, branching, looping, return, and
unreachable are non-memory operations but remain typed and width-explicit.
Every operation has a stable rule identifier, checker judgment, reference
semantics, backend mapping, and focused positive and negative tests. Adding an
operation is an explicit trusted-kernel change.

## Checker

The checker consumes a candidate and recomputes control-flow, dominance,
reaching definitions, initialization, extents, ownership-token balance, and the
range facts required by guards and memory operations. It trusts no optimizer
cache or copied annotation.

The core judgments are:

| Operation | Required judgment |
|---|---|
| projection/access | root live; catalog projection; initialization and access mode valid; exact range within current extent |
| guard | typed predicate; true successor refines only facts implied by that predicate |
| clone/move/drop | allocation class permits operation; affine token transition valid; count cannot overflow or underflow |
| aggregate movement | layouts agree; source/destination initialization and ownership transitions are exact |
| update count | target is catalog metadata; new logical length and capacity are representable; length does not exceed capacity |
| call | structural ABI matches; every owned input and output token balances; effects fit declaration |
| tail call | source tail obligation matched; no residual frame obligation |
| host call | authenticated import and capabilities; buffer extent, lifetime, retention, and reentrancy contract valid |

An inability to establish a judgment is a compiler error. It never creates
authority from a name, source spelling, position, or raw instruction pattern.

## Executable SafeWIR semantics

The trusted design includes a small interpreter for the v1 operation kernel.
It models values, traps, initialized storage, extents, reference counts,
destruction, calls, host-event scripts, and proper-tail dispatch. This serves
three purposes:

1. It makes the SafeWIR semantics executable and reviewable.
2. It differentially checks SafeWIR against emitted Wasm on deterministic
   programs and scripted host interactions.
3. It becomes the before/after oracle for generated optimization tests.

Differential testing is evidence, not a universal proof. The checker and
backend mapping remain security-critical.

## Extensible analysis database

The analysis database contains derived knowledge, not program meaning:

```text
AnalysisKey {
    module_revision,
    analysis_schema_version,
    program_point,
    value_or_object,
}
```

Initial analyses may return `Unknown` for nearly everything. Future analyses
may provide constants, ranges, symbolic inequalities, alias sets, escape,
static uniqueness, alignment, loop induction facts, call effects, and profile
data.

Node identities are never reused across revisions. Flow-sensitive results are
keyed to program points. Whole-revision invalidation is the initial sound
policy; dependency-granular reuse is a later compilation-performance feature.
The checker never reads `AnalysisDb` results. If a pass removes a guard or
ownership operation, the final candidate must still satisfy the checker's own
fixed theory.

## Optimization model

The first production milestone enables no optional optimization passes.
Dynamic bounds guards, runtime reference counts, and value-semantic copy paths
are baseline semantics, not optimizations.

Later, a pass consumes valid SafeWIR plus read-only analysis results and emits a
candidate revision. The framework then:

1. runs the complete safety checker on the candidate;
2. checks the pass's semantic-refinement obligation; and
3. keeps the original revision if either check fails.

Safety checking establishes memory safety. Refinement checking establishes that
the pass did not change allowed observations. Focused tests and fuzzing increase
confidence but are not proofs. Memory-affecting production passes ultimately
require per-transformation validation, a proved rewrite rule, or an equivalent
independently checkable argument.

## WasmIR and backend correspondence

WasmIR is private, ephemeral, and non-optimizing. Only the backend may construct
it, only from `ValidatedSafeWir`, and it is encoded in the same call. No
helper, cache, publisher, diagnostic command, or runtime accepts raw WasmIR as
production authority.

The backend is part of the trusted path. Its operation mapping must be small,
table-driven, and reviewed for offsets, widths, signedness, trap order,
ownership effects, imports, and tail behavior. Complex legalization belongs in
SafeWIR, not hidden in WasmIR emission.

Backend evidence is ranked as follows:

1. Reviewed local mapping from every v1 operation and kernel template.
2. Differential execution against the independent SafeWIR interpreter,
   including adversarial bounds, initialization, RC, and tail probes.
3. Decode-and-match only where its expected mapping is implemented
   independently from the encoder.
4. Wasm structural validation and deterministic encoding.

Shared encoder/matcher logic is self-attestation and cannot establish semantic
correspondence by itself. Exact-byte binding proves that admitted bytes are the
bytes later executed; it does not prove that lowering was correct.

## Migration

Migration maintains one destination and one frozen oracle, not two evolving
compilers.

1. Revert the premature production route flip. Canonical SafeWIR remains an
   explicit development route until cutover.
2. Freeze the current compiler: no new adapter heuristics, name-keyed memory
   contracts, raw-WIR optimizations, or new source semantics on that path.
3. Remove premature optimization, history, candidate-identity, and
   body-result-origin machinery from the canonical critical path. Preserve it
   separately where useful; do not require it for v1 coverage.
4. Implement the v1 affine owned-reference kernel, header bootstrap, local call
   ABI, tail obligations, checker rules, and executable interpreter.
5. Reduce the backend to the reviewed v1 operation mapping and move tail
   legalization out of ad hoc encoding logic.
6. Generate a digest-bound frozen manifest of programs admitted by the frozen
   route, with deterministic inputs and complete observable traces. Classify
   explicit lifetime and no-copy cases as v2 requirements rather than silently
   weakening them.
7. Migrate source lowering by semantic family: scalars and control; strings and
   bytes; scalar lists; pointer-bearing lists; records and sums; dictionaries;
   closures and indirect calls; host capabilities; iterators, generators, and
   async frames.
8. Keep one end-to-end executable SafeWIR probe green for every completed
   family. Missing families fail explicitly in the development route.
9. Cut production over atomically only when the frozen admitted manifest has
   trace parity and required v2 cases are supported. There is no per-program
   fallback.
10. Delete the adapter, name-keyed contract authority, production raw encoders,
    and all WIR optimization after cutover. Rename the remaining private form
    WasmIR.

The production route revert is represented by commits `099446cf9` and
`2702b06b4` on the implementation branch. Focused callable-manifest work that
is not part of the simplified v1 critical path is preserved separately on
`salvage/rfc0165-scalar-callable-abi`.

## First production milestone acceptance

The first milestone is an optimizer-free but useful SafeWIR compiler. It is
complete only when all rows below are green on the cutover candidate.

| ID | Requirement |
|---|---|
| B1 | The v1 operation list and checker judgments are exhaustive and independently reviewed. |
| B2 | Reference-counted pointer-bearing aggregates recursively clone, move, replace, drop, and reclaim correctly; count overflow and underflow reject. |
| B3 | Dynamic indexing keeps runtime guards; header bootstrap cannot authorize body access; logical length never exceeds capacity. |
| B4 | Calls balance owned tokens locally; v1 has no `CallBorrow`; proper tail sites are complete and pass the 300,000-transition probes. |
| B5 | Capabilities are opaque authenticated references; host buffers satisfy retention and reentrancy rules. |
| B6 | The independent SafeWIR interpreter agrees with emitted Wasm for the frozen deterministic corpus and scripted host traces, including adversarial bounds and ownership probes. |
| B7 | WasmIR has no optimization and no production constructor except the reviewed backend mapping from validated SafeWIR. |
| B8 | No optional SafeWIR optimization pass is enabled. Mandatory semantic legalization is explicit and separately evidenced. |
| B9 | The frozen adapter-admitted manifest has complete trace parity; explicit opt lifetime/no-copy requirements are implemented or remain cutover blockers. |
| B10 | The atomic cutover removes every production raw-WIR encoder, adapter authority, mixed helper path, and per-program fallback. |
| B11 | Every executable, cached, published, or launched byte sequence is bound to the exact admitted artifact produced from the checked SafeWIR module. |
| B12 | Native security suites compile and run as mandatory gates; a compile failure is red rather than silently skipping tests. |

Passing B1-B12 supports the conditional memory-safety claim for the
optimizer-free SafeWIR compiler. It does not establish optimization correctness
because no optional optimization is enabled.

## Optimization milestones

After cutover, analyses and passes land independently:

1. constant propagation and interval ranges;
2. guard hoisting and elimination;
3. retain/drop pairing elimination and static uniqueness;
4. allocation and copy elimination;
5. loop and vector transformations;
6. CFG restructuring, inlining, and other call transformations.

Each enabled pass requires safety rechecking, an explicit observation model,
before/after semantic evidence appropriate to its risk, generated composition
testing, and a performance result demonstrating why it should ship. Opt-mode
loans and regions are introduced only when owned views, copies, and dynamic
uniqueness cannot satisfy an existing no-copy contract.

The compiler-wide claim that optimization preserves memory safety requires all
production pass outputs to be independently accepted by the SafeWIR checker.
The stronger claim that optimization preserves program behavior additionally
requires the enabled passes' refinement obligations. Coverage, fuzzing, and
performance results must not be reported as universal proofs.

## Non-goals

- Proving source type checking sound in this RFC.
- Embedding proof terms in SafeWIR.
- Solving every future optimization analysis before baseline cutover.
- Requiring identical collection capacities or allocation growth between
  normal and opt modes.
- Preserving raw-WIR optimization or production compatibility after cutover.

## Drawbacks

This remains a substantial source-lowering rewrite. The affine owned-reference
model and interpreter reduce the trusted conceptual surface, but they do not
remove work needed for records, closures, generators, host calls, recursive
destruction, and opt-mode no-copy contracts. Until atomic cutover, the frozen
compiler remains production while the development SafeWIR route expands.

The first cutover intentionally sacrifices optional optimization throughput to
establish a defensible semantic baseline. Dynamic reference-count uniqueness
and reclamation remain because they are required for usable memory behavior,
not because they are optional optimizations. Near-native opt-mode performance
is a subsequent milestone powered by the same IR and progressively stronger
analysis database.
