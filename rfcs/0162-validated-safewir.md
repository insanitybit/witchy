---
rfc: 0162
title: "Validated SafeWIR for zero-cost object-memory safety"
status: proposed
created: 2026-08-26
superseded-by:
related:
  - "0160 (Zero-cost verified memory safety for generated Wasm)"
  - "0051 (Memory safety by construction — rc invariants, one allocator, and deleting the per-method zoo)"
  - "0127 (Value ownership, mutation, and the opt-mode access contract)"
  - "0135 (Stable observable semantics converge before support)"
  - "0153 (Effect-Safe Range and Loop Optimization: Typed Facts, Trap Preservation, and Reusable Sequence Plans)"
tracking: "Proposal. Supersedes RFC-0160's broader proof-producing encoder and final-byte witness design."
---

# RFC-0162: Validated SafeWIR for zero-cost object-memory safety

## Summary

Witchy will treat the optimizer as an untrusted producer of candidate WIR and
will encode only a final optimized module accepted by a small, fail-closed
object-safety validator.

The validator checks a security-typed WIR dialect, called **SafeWIR**, whose
compiler-only facts identify each linear-memory object's layout, extent,
provenance, ownership state, and lifetime. These facts are not Witchy values and
do not appear in generated WebAssembly. A successfully validated program emits
the same loads, stores, calls, branches, and allocation representation that its
chosen optimization would otherwise emit.

The design has three foundations:

1. SafeWIR retains exactly the facts needed to distinguish a valid object
   access from a type-confused, out-of-object, wrapped, or use-after-free
   access.
2. One mandatory validator independently checks the final combination of all
   Witchy optimization passes. Individual passes do not require formal proofs.
3. Raw linear-memory instructions are confined to a small memory kernel and a
   mechanical SafeWIR-to-Wasm encoder. Ordinary lowering and optimization
   cannot construct them directly.

Historical SEC reproducers, validator mutation tests, differential fuzzing,
heap poisoning, and exact production-pipeline tests close implementation and
specification gaps. A pinned Binaryen build remains an explicit residual
trusted dependency while Witchy continues to run `wasm-opt` after erasing
SafeWIR facts.

This RFC adds no source annotations, proof syntax, runtime tags, canaries,
metadata loads, branches, fields, or dynamic ownership checks.

## Relationship to RFC-0160

RFC-0160 established the correct threat model and zero-runtime-cost goal, but
proposed a broader system: proof-producing analyses, a post-optimization WIR
verifier, a proof-producing encoder, and an independent final-byte witness
checker.

This RFC keeps the threat model, typed memory operations, sealed kernel, final
optimized-WIR validation, and evidence requirements. It deliberately defers:

- general proof-producing optimization passes;
- full pass-by-pass semantic equivalence checking;
- an independent final-byte object-safety checker;
- proof-carrying cache or distribution artifacts; and
- RFC-0160's compiler-process hardening program, including the transitive
  unsafe-code census and parser/compiler resource budgets.

The replacement principle is the one used by validation-a-posteriori systems:
the optimizer may be complicated and unverified, while a much smaller checker
decides whether its particular output satisfies the required safety policy.
RFC-0160 is marked `superseded` by this proposal rather than remaining a second
live proposal for the same decision.

## Motivation

### The source is adversarial compiler input

A dependency granted no capabilities may still choose arbitrary well-typed
source, call shapes, aliases, closures, allocation sizes, control flow, and
optimization triggers. A function as small as `String -> String` can therefore
target a false optimizer fact.

The SEC findings repeatedly exposed three families of compiler defect:

1. **Ownership and provenance:** a value is treated as fresh or unique even
   though it aliases a caller, projection, closure capture, or call argument.
2. **Size and range arithmetic:** allocation or address arithmetic wraps,
   narrows, or loses the relationship between an index and the object length it
   was checked against.
3. **Layout identity:** a specialized, packed, scalar, boxed, closure, or
   reference-bearing value is consumed using a different physical layout.

These are not ordinary wrong-answer bugs when untrusted source controls the
trigger. A false fact can become an out-of-object store or premature release.
The dependency can then groom allocation reuse, replace a publisher-owned path
or request with attacker bytes, and have privileged caller code pass that data
alongside a genuine host-held capability. Unforgeable `externref` capabilities
prevent direct capability manufacture; they do not prevent a corrupted
argument from turning the publisher into a confused deputy.

### Wasm validation is necessary but answers a different question

WebAssembly validation proves Wasm stack, reference, control-flow, and module
typing. It confines a module to its own linear memories and tables. It does not
know that one `i32` is a live Witchy string and another is a freed packed list,
or that a load is outside the logical allocation but still inside the same Wasm
memory.

Current WIR exposes `Load`, `Load8U`, `Store`, `Store8`, `MemoryCopy`, and
`MemoryFill` over ordinary `i32` expressions. By the time those nodes exist,
important freshness, ownership, range, and specialized-layout decisions have
already been made in lowering. The final `wasmparser::validate` call cannot
reconstruct the erased facts.

### Pass combinations should not require a combinatorial proof matrix

It is expensive and unnecessarily restrictive to prove every optimizer
implementation correct. It is also insufficient to validate each pass only
against assumptions that a later pass may invalidate.

Instead, all passes target one closed SafeWIR safety judgment. The final
validator checks the actual combined result. Per-pass validation remains a
development aid for locating the first invalid transformation, not a release
security dependency.

### Tests are evidence, not the foundation

Differential tests, optimization sweeps, heap poisoning, and fuzzing are good
at finding examples. They cannot establish that every accepted memory access
has the required object provenance. The foundation must make that a decidable
compile-time property of final WIR; tests then attack the checker, kernel,
encoder, summaries, and property definition.

## Goals

1. Reject final optimized WIR containing an unproven cross-object access,
   use-after-free, layout confusion, ownership violation, or wrapping object
   calculation.
2. Preserve optimizer freedom: passes may use arbitrary algorithms and may
   compose in any pipeline order allowed by their ordinary dependencies.
3. Add no runtime work or representation to generated Witchy binaries.
4. Impose no proof or annotation burden on Witchy authors or dependency
   publishers.
5. Make optimizer-combination safety a consequence of one final judgment,
   rather than a hand-maintained pass matrix.
6. Shrink the Witchy-specific generated-memory-safety trusted computing base to
   the SafeWIR rules, validator, memory kernel, and encoder.
7. Keep semantic correctness and object-memory safety separate so the critical
   security property can ship without first proving the whole compiler.

## Non-goals

- Proving complete source-to-Wasm semantic equivalence.
- Proving every optimization pass implementation correct.
- Proving Binaryen, Wasmtime, the browser engine, the operating system, or
  hardware correct.
- Protecting arbitrary third-party Wasm that did not pass through the checked
  Witchy source and SafeWIR pipeline.
- Adding runtime redzones, canaries, shadow memory, type tags, ownership checks,
  or pointer capabilities to production binaries.
- Replacing required source-semantic checks. For example, a dynamic list access
  still traps when its index is invalid unless a valid optimization proves the
  check unnecessary.
- Preventing resource exhaustion within configured fuel, memory, recursion,
  compiler, and host-operation budgets.
- Making leaks memory corruption. Conservative retention is safe for this RFC,
  though leaks remain correctness and resource defects.

## Threat model and safety claim

The attacker controls a dependency's Witchy source but receives no capability
parameter and no corresponding host import. The dependency is linked into the
same compiled module as publisher code that may hold capabilities. The attacker
may choose source constructs and inputs specifically to trigger compiler edge
cases and may repeat execution to groom the guest allocator.

The trusted launch path supplies checked Witchy source to the canonical
compiler entrypoint. Precompiled arbitrary Wasm is outside this claim. Host
capabilities remain unforgeable references, and host handlers retain their
independent rights, path, policy, and bounds checks.

For every execution of an accepted module before any unvalidated external
postprocessor:

- every linear-memory read and write is within a live object and within the
  byte range authorized by its exact layout;
- every object address and byte-length calculation is non-wrapping at the
  width where it is performed and valid before any narrowing conversion;
- every field, element, header, retain, release, copy, move, and reuse operation
  agrees with the object's `LayoutId` and ownership state;
- an object's storage is not accessed after its lifetime token is consumed;
- a destructive operation cannot consume storage while a live alias or borrow
  remains;
- bulk operations validate both source and destination ranges and preserve the
  selected overlap semantics;
- scalar linear-memory contents cannot be reinterpreted as an `externref`, GC
  reference, capability, closure environment, or layout identity; and
- host-call buffer arguments name live ranges with the checked ABI
  representation.

The claim permits compile-time rejection, safe traps required by Witchy
semantics, and conservative retention. It does not claim that every compiler
bug is impossible: bugs outside the memory-safety judgment may still produce a
wrong value or wrong allowed trap, and the validator, kernel, encoder, and
explicit external-tool TCB remain security-critical.

## Design

### 1. Two authority levels: candidate and validated WIR

Production APIs distinguish modules that merely exist from modules authorized
for encoding:

```text
CandidateWirModule
    |
    | optimize freely
    v
CandidateWirModule
    |
    | validate_safewir
    v
ValidatedWirModule
    |
    | encode
    v
WebAssembly bytes
```

`ValidatedWirModule` has a private constructor. The production encoder accepts
only that type. Test-only forged-WIR entrypoints remain behind test
configuration and cannot be linked into ordinary compiler or runtime builds.

Validation occurs after every Witchy AST, lowering, specialization, WIR,
inlining, tail-call, and peephole optimization that can affect the module.
Development builds and tests may validate between passes to report the first
offender, but only the final check is required for composition.

### 2. SafeWIR ghost facts

SafeWIR retains a compact static description for linear-memory values:

```text
ObjectRef {
    root: DefinitionId,
    layout: LayoutId,
    extent: Extent,
    provenance: Provenance,
    ownership: Ownership,
    generation: GenerationId,
}

Place {
    object: ObjectRef,
    projection: Projection,
    layout: LayoutId,
    range: ByteRange,
    access: AccessMode,
}

ByteRange {
    object: ObjectRef,
    start: SizeExpr,
    length: SizeExpr,
}
```

`DefinitionId`, `GenerationId`, provenance edges, ownership states, layout IDs, and
symbolic extents exist only in the compiler. At runtime an `ObjectRef` remains
the same `i32` pointer used today, and a `Place` remains the same address
expression.

The minimum ownership lattice is:

```text
Unique
Shared
BorrowedRead
BorrowedWrite
Consumed
```

The minimum provenance lattice is:

```text
Fresh(root)
Aliases(root, projection)
MayAlias(set of roots)
StaticData(root)
Unknown
```

`Unknown` is valid for values that are not used destructively. It never
satisfies uniqueness, freshness, early-release, unchecked projection, or
destructive-reuse obligations.

#### Mapping facts onto mutable WIR

Executable WIR retains named mutable locals: `GetLocal(String)` and
`SetLocal`. SafeWIR does not pretend those names are SSA values. The validator
builds an analysis-only def-use graph without rewriting executable WIR. Every
parameter and every local write, including each `SetLocal` and multi-result
call destination, receives a `DefinitionId`; a value-producing SafeWIR
expression used without a local receives an ID for its expression site. Each
`GetLocal` resolves to its reaching definition set; control-flow joins receive
a synthetic `DefinitionId`, and loop headers are solved to a conservative
fixpoint. A fact survives a join only when it is valid for every reaching
definition.

`DefinitionId` identifies a WIR value version. `GenerationId` separately
identifies object storage lifetime and is minted by allocation or destructive
reuse even when the numeric address is unchanged. Thus `x = f(x)` cannot reuse
facts attached to the pre-call definition of `x`, and an alias returned by `f`
cannot become fresh merely because it is assigned to a new local definition.

The numeric component extends RFC-0153's `RangeFact` lattice; it does not create
a competing range lattice. RFC-0153 facts move from local-name keys to
`DefinitionId` keys, while SafeWIR augments them with the exact object
generation and length-source definition to which a bound applies.

### 3. Semantic object operations

Ordinary lowering and optimization use operations whose safety obligations are
explicit:

```text
Alloc(layout, count) -> ObjectRef
Project(object, field) -> Place
CheckedIndex(object, index, length_source) -> Place
ProvenIndex(object, index, range_fact) -> Place
Load(place) -> Value
Store(place, value)
Copy(source_range, destination_range, copy_kind)
Fill(destination_range, byte)
Retain(object) -> SharedObject
Release(object)
Move(object) -> UniqueObject
Reuse(object, operation) -> ObjectRef
```

This split does not force every dynamic safety property to be statically
discharged:

- `CheckedIndex` encodes the same semantic bounds check the unoptimized program
  requires.
- `ProvenIndex` emits the existing unchecked fast form but validates a range
  implication tied to the exact object and length source.
- `Alloc` uses the canonical allocator and its existing allocation-failure and
  size semantics.
- an allocation-size optimization may emit a faster known-size form only when
  its non-wrapping size expression validates;
- shared RC operations retain their existing runtime RC behavior; only an
  optimization that removes, moves, or replaces them needs static ownership
  evidence; and
- Wasm GC array and struct operations continue to use native reference typing
  and bounds semantics. SafeWIR validates their exact type identity but does not
  replace Wasm's checks.

No new check is inserted to compensate for an optimizer. The validator either
accepts the optimized form already selected or rejects compilation.

### 4. Function ownership and alias summaries

SEC-078 demonstrates that `result of a call` is not synonymous with `fresh`;
its named regression is
`src/example_tests/region.rs::rc_free_at_overwrite_does_not_free_an_aliased_call_result_sec_078`
on `security/fix-sec078-call-result-uaf`. SafeWIR therefore gives every
memory-bearing callable a checked summary:

```text
CallableMemoryContract {
    parameters: [Read | Write | Consume | Retain | Escape],
    result: Fresh(layout)
          | Aliases(parameter, projection)
          | Shared(parameters)
          | NoMemory
          | Unknown,
}
```

The validator checks the declared summary against the final callable body.
Recursive components are solved to a conservative fixpoint. Imported compiler
primitives receive catalogued contracts checked against their kernel or host
definitions.

Missing information defaults to `Unknown`; it never defaults to `Fresh`.
Consequently a dependency function returning its input remains safe without
annotations: the result may be read, returned, or retained, but cannot authorize
free-at-overwrite or destructive reuse of the caller's storage.

Optimizers may infer and exploit more precise contracts. A stale summary is not
trusted merely because it is attached to a function: validation recomputes or
checks it against the final body and call graph.

### 5. Range, arithmetic, and dominance rules

The validator uses mathematical size expressions until an explicit narrowing
operation. It checks:

- allocation header plus payload size;
- element count multiplied by stride;
- base plus field, element, or slice offset;
- start plus length for both sides of a bulk operation;
- full-width signed-index rejection before conversion to Wasm `i32`; and
- consistency between an index fact and the exact object generation and length
  source it constrains.

A dynamic range is safe by one of two routes:

1. the semantic WIR operation retains the required check; or
2. a dominating branch/range fact proves the optimized unchecked access.

Range facts carry the defining `DefinitionId`s and object generation. Reassignment,
memory growth that changes a length source, object reuse, control-flow merge,
or replacement of a defining expression invalidates facts that depend on it.
The validator derives validity from the final control-flow graph; an optimizer
cannot make a stale fact valid by copying its identifier.

### 6. Ownership and control-flow rules

Ownership state is dataflow-checked over structured WIR:

- allocation mints one `Unique` generation;
- sharing or retaining transitions to `Shared` according to the operation's
  contract;
- borrows are bounded by their lexical or control-flow lifetime;
- `Move`, destructive `Reuse`, and unique `Release` consume the previous token;
- reuse mints a new generation even when the numeric address is unchanged;
- a consumed generation cannot reach a later memory operation; and
- control-flow joins retain only facts valid on every incoming edge.

Loops are checked at a fixpoint. A loop-carried unique value must return one
compatible unique generation to the header on every back edge; otherwise it is
treated as shared or rejected for destructive operations. The checker need not
predict concrete pointer values or allocation counts.

Leaks are a safe fallback. When ownership evidence is insufficient, retaining
storage is valid; freeing or overwriting it is not.

### 7. Layout and reference rules

SafeWIR reuses the canonical `LayoutId` and descriptor graph already used for
specialized layouts and host-layout contracts.

- `Project` derives its result layout and offset from the canonical descriptor.
- `Load` and `Store` widths follow the projected layout, not an independently
  supplied `Kind`.
- packed-list indices use the descriptor's element layout and stride.
- constructors, sums, tuples, closures, callable carriers, and boundary adapters
  must agree on the same concrete layout identity.
- conversions between linear-memory objects and Wasm reference kinds do not
  exist.
- `externref` and concrete GC references remain reference-typed through locals,
  calls, tables, structs, arrays, and host imports, with standard Wasm validation
  as an independent backstop.

An unknown or conflicting layout is a compile-time rejection. There is no
fallback to scalar slots, generic boxed storage, or a nearby expression's
shape at the validated boundary.

### 8. The sealed memory kernel

This RFC chooses an **opaque-operation, inline-encoding boundary**. Allocation,
retain, release, free-list, region-watermark, and unavoidable byte primitives
remain first-class SafeWIR operations through optimization and final
validation. Their implementations are not ordinary `WirFunc` bodies and cannot
be selected by `wir_opt::inline::do_inlining`.

After validation, the trusted encoder expands each operation at its use site
using a fixed kernel template. The generated Wasm therefore need not contain a
helper call: the current call-elimination benefit can be preserved. What is
deliberately lost is optimizer visibility into the raw loads and stores inside
the template. Encoder expansion is part of the mechanical SafeWIR-to-Wasm
mapping, not a post-validation optimization, and it cannot inspect surrounding
user WIR to choose an unchecked variant.

Raw linear-memory constructors consequently become private to the encoder's
kernel-template implementation. The validator checks each opaque operation's
typed preconditions; it does not validate the raw template body. Those fixed
templates and the encoder remain the trusted memory kernel.

String, byte, list, dictionary, record, closure, and numeric helpers are not
automatically kernel code merely because they currently contain loads and
stores. They migrate to SafeWIR object operations. The kernel is the minimal
code that implements those operations' representation rather than an allowlist
of existing helper files.

A workspace structural test rejects raw load, store, copy, fill, heap-global
write, or raw-body construction outside the named encoder kernel-template
module. It also rejects ordinary `WirFunc` definitions of privileged kernel
operations, preventing the general inliner from smuggling raw operations into
user functions. Existing helpers migrate to opaque operations rather than
being grandfathered into the kernel.

Adding a new privileged operation requires an explicit kernel contract and
negative test.

RFC-0051 established that inlined RC paths are performance-load-bearing. The
zero-cost gate therefore separately records the emitted calls, loads, stores,
dynamic instruction count, and protected benchmarks for `rc_dup`, `rc_drop`,
`rc_free`, leaf retain/release, allocation, and raw-buffer copy before and after
this boundary change. Any regression blocks acceptance. Permitted remedies are
more precise typed operations or fixed encoder templates; accepting raw
kernel-expanded regions in user WIR is not a remedy under this RFC.

The first implementation keeps these templates in the trusted computing base,
audited directly and exercised with boundary, poisoning, reuse, exact-encoding,
and mutation tests. Mechanically proving their contracts is a possible later
strengthening, not an acceptance requirement for this RFC.

### 9. Validation algorithm and failure policy

`validate_safewir` performs:

1. canonical layout and callable-contract validation;
2. local and function signature typing;
3. provenance and ownership dataflow;
4. range, dominance, and non-wrapping checks;
5. memory-operation and host-ABI checks; and
6. structural confirmation that no privileged raw operation bypasses the
   dialect.

The checker is deterministic, has explicit recursion and fact budgets, and
fails closed on exhaustion. It produces diagnostics naming the function,
operation, missing judgment, and responsible optimization when development
attribution is available. Diagnostic attribution is not trusted for safety.

The validator accepts or rejects; it does not repair candidate WIR. Repairing
or silently deoptimizing after validation would create a second transformation
that itself needed validation. A pass that cannot preserve evidence must emit a
safe semantic form before the final check.

The compiler may retain the pre-optimization semantic-form module as a distinct
candidate. If optimized validation fails or exhausts an optimizer-complexity
budget, it may discard that candidate and independently validate the retained
semantic-form module from its entry point. It may not rewrite the rejected
module into that form. If the semantic candidate also exhausts a budget,
compilation fails with the named limit; a user or build operator may retry with
a larger compile-time validation budget, but no path encodes unchecked WIR.

### 10. Mechanical encoding and Wasm validation

The encoder accepts only `ValidatedWirModule`. Each SafeWIR operation maps to a
small fixed encoding template, including the inline kernel templates specified
in section 8, and ghost facts are ignored rather than encoded. This fixed
expansion is the only transformation after validation. The encoder cannot
accept arbitrary raw pointer expressions from ordinary lowering.

After encoding, `wasmparser` validation remains mandatory. It independently
checks Wasm types, references, structured control flow, imports, tables, GC
types, and binary well-formedness.

This RFC does not require a second checker to reconstruct Witchy object
provenance from final Wasm bytes. The encoder and kernel therefore remain in
the trusted computing base. Their narrow interface, structural census, exact
encoding tests, standard Wasm validation, and SEC corpus are the selected
engineering tradeoff.

Production raw-body functions are prohibited. Test-only raw-body fixtures stay
behind test configuration and never cross the `ValidatedWirModule` boundary.

### 11. External Wasm optimization

Witchy currently may run Binaryen `wasm-opt` after WIR encoding. Binaryen can
change address arithmetic and memory operations after SafeWIR facts have been
erased, so pre-Binaryen validation cannot remove it from the trusted computing
base.

Current code already hashes `BINARYEN_RECIPE_SCHEMA` and the output of
`wasm-opt --version` with the input Wasm for optimized-cache identity. If the
tool is unavailable or fails, `binaryen_optimize` already returns the
unprocessed input. This RFC preserves those properties. The remaining identity
gap is that both the version probe and optimizer invocation resolve an
arbitrary `wasm-opt` from `PATH`; the remaining evidence gap is the exact
pre/post production-pipeline differential corpus.

For the assurance claim in this RFC:

- releases use a pinned Binaryen version and executable identity, not an
  arbitrary `wasm-opt` resolved from `PATH`;
- the existing exact production recipe and reported version continue to
  participate in artifact and cache identity;
- malformed, unavailable, or unexpected optimizer output continues to fail
  closed or use the already-validated unprocessed module according to the
  explicit launch policy;
- every SEC fixture and generated adversarial module is differentially executed
  before and after the exact production Binaryen pipeline; and
- modules marked `PreserveRaw` never pass through Binaryen.

This is an explicit residual trust decision, not a proof that Binaryen cannot
miscompile Witchy. Removing it from the object-safety TCB would require a future
post-Binaryen validator, a validated optimizer, or migration of its required
transformations into SafeWIR. Tests provide confidence in this residual gap;
they do not turn it into a universal guarantee.

### 12. Zero-runtime-cost contract

SafeWIR validation is permitted to increase compiler time and compiler memory.
It must not add to production execution:

- instructions or branches;
- memory accesses or allocations;
- imports, globals, tables, or functions;
- allocation headers, object fields, type tags, canaries, or redzones;
- shadow state or metadata reads; or
- dynamic ownership or layout checks.

Ghost facts, contracts, object IDs, generations, and proof diagnostics are
erased before encoding. Existing checks required by Witchy semantics remain.
An optimization that validly removes one still emits its existing fast form.

The implementation gate compares generated modules before and after the
SafeWIR migration for operations whose optimization decision is unchanged. It
also records code size, dynamic instruction count, allocations, and protected
benchmark performance. A safety feature that requires new runtime work needs a
different RFC and does not satisfy this one.

For opaque kernel operations, the comparison includes exact WAT and dynamic
instruction evidence that encoder expansion preserved the present inlined hot
paths. Equal call count alone is insufficient: extra loads, stores, branches,
or lost constant folding fail the gate even when no helper call survives.

## Testing and adversarial evidence

### Validator rule tests

Every validation rule requires at least one accepted fixture and one rejected
fixture. Negative fixtures cover:

- forged fresh results and stale callable summaries;
- aliases through parameters, projections, closures, containers, and recursive
  calls;
- duplicate, consumed, and mismatched ownership generations;
- release or destructive reuse with a live borrow or alias;
- wrong field, element, header, scalar width, or `LayoutId`;
- signed and unsigned boundary errors, multiplication overflow, narrowing, and
  start-plus-length wrap;
- range facts tied to the wrong object, generation, index, or length source;
- invalid control-flow joins and loop-carried ownership;
- bulk-copy source, destination, overlap, and ownership errors;
- reference/scalar storage confusion; and
- host-call buffer and ABI mismatches.

Mutation tests delete, invert, or weaken each validator rule. A rule without a
test that kills its mutation is not accepted.

### SEC corpus

Every applicable generated-memory-safety SEC finding becomes:

1. a source-level interpreter versus optimized-Wasm regression;
2. a forged candidate-WIR negative test rejected for the named invariant; and
3. where the defect touched the kernel or encoder, a focused boundary or byte
   encoding test.

SEC identifiers have been reused across evaluation series, so acceptance
evidence always records an ID, title or class, and path or test name. The
initial applicable generated-object-memory corpus is:

| Finding | Named evidence and current gap | SafeWIR obligation |
|---|---|---|
| SEC-036/SEC-037, RC non-object/interior-pointer use-after-free class | `tests/differential_fuzz.rs::uaf_sanitizer_is_false_positive_free` and `rc_assert_dup_drop_is_false_positive_free`; RFC-0051 records the historical class, but the distinct SEC-036 report/reproducer is not present on current master | Restore a dedicated source reproducer for SEC-036 or record it as the same trigger as SEC-037; reject forged retain/release on an interior or non-owning pointer |
| SEC-038, unchecked `bytes.at` out-of-object read | `src/example_tests/bytes.rs::bytes_index_out_of_bounds_errors_on_both_backends` | Reject a forged unchecked byte load without an exact object-bound range fact |
| SEC-039, free-at-overwrite of a non-object literal | `src/example_tests/region.rs::rc_free_at_overwrite_does_not_free_a_literal_sec_039` | Reject release or reuse of static-data/non-owning storage |
| SEC-078, aliased call result freed at overwrite | Pending regression `src/example_tests/region.rs::rc_free_at_overwrite_does_not_free_an_aliased_call_result_sec_078` on `security/fix-sec078-call-result-uaf` | A call returning its `String` argument cannot authorize freeing or overwriting the caller's retained alias |

The August 23 offensive series `SEC-001..SEC-015` is a separate namespace. Its
SEC-013 forged `vm.par_map` count is a hostile-precompiled-Wasm host-allocation
bug, retained as
`crates/witchy-runtime/src/runtime_tests.rs::forged_par_map_count_is_bounded_before_host_allocation`;
it is a host ABI/resource boundary rather than generated intra-object memory
safety. SEC-008 cache substitution is governed by accepted RFC-0157. The other
items in that series are cache, publication, ambient-authority, network, or
resource findings and do not enter this corpus.

The `SEC-040..SEC-045` labels likewise cover mixed wrong-answer, parser-process,
network, and standard-library findings. They remain in their owning suites but
are not object-memory evidence merely because they were found during compiler
parity work. In particular, SEC-041 parser depth belongs to the compiler-process
hardening deferred from RFC-0160.

### Generative and differential testing

The typed-program generator emphasizes combinations that historically defeated
local reasoning:

- calls and call results around reassignment;
- nested projections and containers;
- closure capture and indirect calls;
- loops, early exits, matches, and recursive components;
- zero, maximum, negative, narrowing, and page-boundary sizes;
- packed, boxed, scalar, GC-reference, and specialized layouts; and
- optimization toggles and legal pass combinations.

Each generated program compares the interpreter, safe semantic Wasm, optimized
Wasm before Binaryen, and production-pipeline Wasm. The existing RFC-0023 CI
`heap-check` job and `heap_check_enabled`/`uaf_check_enabled` modes are extended
with immediate same-size reuse, stronger poison fill, and randomized allocation
placement. These instruments never enter production artifacts.

### Calibrated offensive campaigns

Before claiming a material exploit-cost increase, the project reruns bounded
hostile-source campaigns against a frozen release. Campaigns include withheld
historical mutations to establish that the method can rediscover known classes.
Results distinguish crashes, semantic divergences, object-memory primitives,
and end-to-end capability use.

No fixed campaign budget proves absence. Repeated failure to find a primitive,
combined with complete invariant and mutation coverage, is evidence about
residual bug density rather than a mathematical guarantee.

## Delivery plan

Implementation follows the dependency order below:

```text
canonical LayoutId graph and checked callable types
                        |
                        v
        SafeWIR object and ownership judgments
                        |
                        v
             final validator in report mode
                        |
             +----------+----------+
             |                     |
     migrate ordinary        shrink raw-memory
     memory operations       kernel boundary
             |                     |
             +----------+----------+
                        v
          mandatory ValidatedWirModule gate
                        |
                        v
          mechanical encoder + Wasm validation
                        |
                        v
           pinned Binaryen + differential corpus
```

The migration is incremental but the security claim is not:

1. Inventory every production raw memory constructor, heap-global write,
   callable memory summary, specialized layout boundary, and raw-body path. The
   2026-08-26 baseline is 478 raw memory-operation construction sites across 25
   non-test files when alias forms such as `E::Load` are counted: roughly 400
   are under `wir_helpers` (`dict.rs` 112, `strings/transform.rs` 44,
   `bytes.rs` 37), roughly 78 are under lowering/codegen, and optimizer files
   contain two actual construction sites rather than merely match arms.
2. Add ghost object, layout, provenance, range, and generation identities
   alongside existing WIR without changing encoded bytes.
3. Implement the validator in report mode and freeze its diagnostic census.
4. Migrate ownership/reclamation, size/index arithmetic, and specialized-layout
   families, in that priority order.
5. Move non-kernel helpers to SafeWIR operations and enforce the raw-constructor
   structural boundary.
6. Make validation fail closed and make the encoder accept only
   `ValidatedWirModule`.
7. Pin and exercise the production Binaryen pipeline.
8. Complete mutation, SEC, zero-runtime-footprint, performance, and adversarial
   gates before promoting the release claim.

Report mode is development scaffolding only. Until step 6 is complete, Witchy
does not have the SafeWIR guarantee.

## Acceptance ledger

No row may be marked complete from prose, a green workspace gate, backend
agreement alone, or a subset of optimizer configurations.

| Contract | State | Required evidence |
|---|---|---|
| Production codegen has one checked source entry | pending | Raw entrypoints are test-only; missing or contradictory source types and layouts reject |
| SafeWIR represents object identity, layout, extent, provenance, ownership, and generation | pending | Accepted and forged fixtures for every judgment family |
| Call results default to possible aliasing | pending | SEC-078 plus direct, indirect, recursive, closure, projection, and imported-call matrices |
| Final optimized WIR is checked once, fail closed | pending | Private `ValidatedWirModule` constructor and production encoder API test |
| Optimizer combinations cannot retain stale facts | pending | Reassignment, join, loop, inlining, specialization, and pass-combination negative tests |
| Raw memory is sealed | pending | The 478-site baseline reaches zero constructors outside the encoder kernel-template module; ordinary privileged `WirFunc` helpers cannot exist or inline |
| Kernel is small and directly tested | pending | Published operation census, contracts, poisoning, boundary, reuse, and mutation coverage |
| Encoder mechanically erases SafeWIR facts | pending | Exact template tests plus standard Wasm validation and no production raw-body path |
| Every applicable SEC reproducer is permanent | pending | The enumerated SEC table has a landed source regression and forged-WIR rejection for every row; SEC-036 is restored or explicitly deduplicated |
| Validator rules resist implementation mistakes | pending | Mutation matrix with every rule killed |
| Binaryen trust is explicit and reproducible | pending | Pinned identity, exact flags, cache identity, pre/post differential corpus, fail-closed fallback |
| Generated execution has zero added safety overhead | pending | Instruction, branch, access, allocation, import, global, field, code-size, and benchmark comparison |
| Release claim is fail closed | pending | Deliberately invalid final WIR cannot encode through any production path |

`implemented` requires every row above. A report-only validator, typed nodes
without a mandatory gate, or a validator bypassed by a production raw path is a
partial implementation.

## Alternatives

### Prove every pass correct

Deferred. CompCert and CakeML show that this can provide very strong guarantees,
but the implementation and maintenance cost is unnecessary for the first
Witchy safety property. Validating each actual result permits complex and
replaceable optimization algorithms.

### General source-to-target translation validation

Deferred. Alive2-style equivalence checking is valuable for semantic
miscompilation but is bounded, incomplete, and substantially broader than the
object-memory-safety property. SafeWIR validation checks a smaller closed
judgment; differential tests remain the semantic oracle.

### Validate every optimization pass in production

Not required. It improves diagnostics but does not strengthen the final safety
claim if the final validator independently checks the combined output. Tests
may enable it to identify the first invalid pass.

### Trust typed builders without a final validator

Rejected. Optimizers mutate and rebuild IR, facts become stale across pass
composition, and test-only or future constructors can bypass builder intent.
The final checker is the authority.

### Rely on tests alone

Rejected as the foundation. The SEC campaign demonstrates both the value and
the incompleteness of testing: it finds concrete failures cheaply but cannot
quantify over all accepted accesses.

### Keep raw WIR and reconstruct object safety afterward

Rejected. Current pointer expressions have already erased allocation identity,
ownership generations, alias provenance, and the exact length source behind a
range fact. Reconstruction would be more complex and less reliable than
preserving those facts through optimization.

### Independently validate final Wasm bytes

Deferred. This would remove more encoder and postprocessor code from the trusted
computing base but requires recovering or carrying object provenance through
arbitrary Wasm transformations. A sealed mechanical encoder, Wasm validation,
and explicit Binaryen TCB are the selected minimum.

### Add runtime object capabilities or tags

Rejected for this RFC. MSWasm-style runtime enforcement, shadow memory,
fat pointers, generation tags, and dynamic ownership checks would strengthen
the executed representation but violate the zero-runtime-cost requirement.

### Move every object to Wasm GC

Deferred as an independent representation optimization. Wasm GC naturally
provides typed references, bounds, and managed lifetimes for supported shapes,
but strings, byte buffers, regions, host staging, and linear-memory kernels
remain. A wholesale migration also needs its own performance evidence.

### Disable risky optimizations

Rejected as the architecture. Temporary release containment for a confirmed
bug remains appropriate, but the long-term design must validate optimized
forms rather than trade away Witchy's performance model.

## Drawbacks

- Current WIR exposes raw memory operations widely, so reaching the sealed
  boundary requires a nontrivial migration.
- Interprocedural alias summaries, loops, and ownership joins make the validator
  more complex than an ordinary Wasm type checker.
- Conservative loss of evidence can turn a compiler bug into a compile-time
  rejection. That is safe but still a user-visible compiler defect.
- The validator becomes security-critical and needs strict mutation, review,
  and compatibility discipline.
- The kernel and encoder remain trusted rather than independently verified.
- Pinned Binaryen remains a large residual trusted dependency after SafeWIR
  erasure; differential tests lower confidence risk but cannot prove it sound.
- Compile time and compiler memory use will increase even though generated
  execution does not.
- Separating object-memory safety from complete semantic equivalence means a
  validated module may still contain a non-memory-unsafe wrong-answer bug.

## Prior art

- **PNaCl and Native Client:** a restricted portable IR was checked by an ABI
  verifier described as the source of truth, and final native code was checked
  against an SFI policy before execution. The transferable lesson is to validate
  a closed output language rather than trust every producer pass.
  <https://chromium.googlesource.com/website/+/ffb1cb0132f6ab5393db77d755855f059e4d572f/site/nativeclient/pnacl/stability-of-the-pnacl-bitcode-abi/index.md>
- **Typed Assembly Language:** type-preserving low-level transformations retain
  high-level abstractions while admitting ordinary compiler optimizations.
  <https://www.cs.cornell.edu/talc/papers/tal-toplas.pdf>
- **CompCert validation a posteriori:** complicated allocation algorithms may
  remain unverified while a smaller checker validates each produced result.
  <https://radar.inria.fr/report/2013/gallium/uid33.html>
- **RockSalt:** a small, declaratively generated, mechanically justified NaCl
  checker reduced the validator trusted computing base and demonstrated that
  apparently simple safety validators still deserve exceptional scrutiny.
  <https://www.cs.cmu.edu/~15811/papers/rocksalt.pdf>
- **MSWasm:** object provenance, spatial bounds, and temporal identity can form
  a low-level robust memory-safety type system. SafeWIR borrows the static
  abstraction but erases it rather than adding runtime enforcement.
  <https://www.microsoft.com/en-us/research/publication/mswasm-soundly-enforcing-memory-safe-execution-of-unsafe-code/>
- **Alive2:** bounded translation validation demonstrates the value of checking
  optimization results independently, while its broader equivalence goal is
  intentionally outside this RFC's minimum.
  <https://web.ist.utl.pt/nuno.lopes/pubs.php?id=alive2-pldi21>
- **Csmith and Binaryen fuzzing:** randomized, differential execution finds
  optimizer bugs missed by conventional suites and remains necessary evidence
  around a typed foundation.
  <https://users.cs.utah.edu/~regehr/papers/pldi11-preprint.pdf>
  <https://github.com/WebAssembly/binaryen/wiki/Fuzzing>

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
-->
