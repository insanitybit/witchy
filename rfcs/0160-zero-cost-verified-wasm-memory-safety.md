---
rfc: 0160
title: "Zero-cost verified memory safety for generated Wasm"
status: superseded
created: 2026-08-26
superseded-by: "0162-validated-safewir"
related:
  - "0051 (memory-safety invariants and the RC floor)"
  - "0127 (ownership, references, layouts, and opt mode)"
  - "0135 (compiled-Wasm correctness and differential evidence)"
  - "0153 (typed optimization facts and conservative fallback)"
tracking: "Superseded by RFC-0162, which keeps the zero-cost object-memory-safety goal and replaces this broader proof-producing/final-byte design with final SafeWIR validation."
---

# RFC-0160: Zero-cost verified memory safety for generated Wasm

> Superseded by RFC-0162. This file remains the record of the broader
> proof-producing and final-byte-witness proposal.

## Summary

Witchy will reject any compiled module for which the compiler cannot prove the
memory-safety and type-soundness invariants of the final generated WebAssembly.
The proof is performed at compile time and adds no instructions, branches,
metadata reads, imports, globals, or heap fields to production execution.

The design has three independently enforced boundaries:

1. The compiler implementation remains memory-safe Rust with an explicit,
   minimized dependency trusted computing base.
2. Only a checked, fully typed program can enter lowering, and lowering carries
   type, layout, provenance, and ownership evidence rather than reconstructing
   it from names or shapes.
3. A post-optimization WIR verifier and a final-byte witness checker reject any
   module whose loads, stores, copies, allocation arithmetic, layouts, or
   ownership transitions are not proven safe.

An optimization may consume a proof and emit the fast form, or decline and use
the safe baseline form. It may not make a speculative transformation and add a
release-mode guard to compensate. Proof objects and diagnostic counters are
compiler-only data and are erased before execution.

## Motivation

WebAssembly validation is necessary but not sufficient for Witchy memory
safety. It proves that a module is structurally valid and that instructions
have compatible Wasm stack types. It does not prove that an `i32.load` addresses
the correct live Witchy allocation, that `memory.copy` stays within two logical
objects, that an `i32` offset did not wrap, that a packed layout matches the
value being read, or that an optimizer did not reuse storage while an alias was
still live.

WIR currently exposes raw `Load`, `Load8U`, `Store`, `Store8`, `MemoryCopy`, and
`MemoryFill` nodes whose addresses are ordinary `i32` expressions. The current
`wasmparser::validate` call cannot distinguish a valid object access from a
well-typed use-after-free. Existing structural invariants such as RFC-0051's
single allocator close important classes, but they do not establish a complete
proof over every memory operation.

This matters at the capability boundary. A dependency with no granted
capabilities must not gain authority by inducing a compiler miscompilation,
corrupting another Witchy value, forging a legacy handle, or changing a host
call's arguments. A source function as small as `String -> String` is therefore
an adversarial compiler input, not a trusted implementation detail.

Testing alone cannot make a universal claim. Differential execution, fuzzing,
heap poisoning, and SEC regressions are required evidence, but they sample
programs. The production rule must instead be fail-closed: no proof means no
module.

## Goals

1. Make generated Wasm object-memory-safe for every accepted, well-typed
   Witchy program, including malicious dependency source.
2. Make type, layout, provenance, and ownership mismatches compile-time
   rejections rather than generated-code behavior.
3. Validate after every Witchy optimization and after any binary postprocessor,
   against the exact bytes that will execute.
4. Add zero runtime cost for verification and proof transport.
5. Preserve Witchy's existing source semantics, including required traps and
   their ordering.
6. Keep the verification core small enough for direct adversarial review,
   mutation testing, and eventually independent reimplementation.
7. Treat every confirmed compiler memory-safety finding as a permanent negative
   test and a release blocker until its invariant is represented.

## Non-goals

- Proving Wasmtime, the browser engine, the operating system, or hardware free
  of implementation vulnerabilities.
- Preventing denial of service within configured fuel, memory, and time limits.
- Replacing Wasm validation, capability import validation, or unforgeable host
  handles. Those remain independent layers.
- Formally verifying the whole compiler in a proof assistant in the first
  implementation.
- Reproducing compiled representations in the interpreter. This is a compiled
  backend safety contract under RFC-0135.
- Adding release-mode redzones, canaries, shadow memory, dynamic type tags, or
  ownership assertions.
- Declaring all arbitrary third-party Wasm safe. The claim covers Wasm accepted
  through the Witchy checked-compilation pipeline.

## Terminology and safety claim

### Compiler memory safety

Compiling any byte sequence as Witchy source must not cause undefined behavior
in the compiler process. Compiler-owned crates deny Rust `unsafe` code. Any
transitive dependency that executes `unsafe` code while parsing, checking,
optimizing, encoding, or validating adversarial input is part of a named
trusted computing base and receives explicit review, pinning, and fuzz coverage.

This is a statement about memory safety, not total correctness. A compiler may
reject valid source, exhaust a configured resource budget, or report an
internal error without violating this property.

### Type soundness

If source passes the Witchy checker, every value consumed by lowering has one
authoritative type and representation. Monomorphization, trait resolution,
closure conversion, packed layouts, callable ABIs, and host imports preserve
that judgment. Missing or contradictory evidence rejects compilation. No stage
may guess a type from an identifier spelling, reconstruct it from a nearby AST
shape, or silently substitute a scalar representation.

### Generated-Wasm memory safety

For every reachable execution of an accepted module:

- every linear-memory read and write lies within a live allocation and within
  the field, element, header, or byte range authorized by its layout;
- every address and byte-length calculation is proven not to wrap or truncate;
- every typed load, store, copy, retain, release, move, and projection agrees
  with the exact `LayoutId` of the value;
- no operation reads or writes an allocation after its lifetime ends;
- no release can free storage that remains reachable through another owner or
  borrow;
- bulk copies prove both source and destination ranges, with overlap behavior
  matching the selected operation;
- scalar values cannot be reinterpreted as owning pointers, GC references,
  capabilities, closure environments, or layout identities; and
- every host call receives arguments with the checked ABI and representation.

Leaks and bounded compiler rejection are not memory corruption. Leaks remain
correctness and resource defects and retain their existing tests, but a proof
may conservatively keep storage live rather than free it early.

## Design

### 1. One checked entry to lowering

The type checker produces an opaque `CheckedProgram` value. Its fields are not
publicly constructible outside the checker boundary. It contains stable
expression and definition identities plus the authoritative judgments needed
by later stages:

```text
CheckedProgram {
    program,
    expression_types,
    resolved_calls,
    callable_abis,
    ownership_obligations,
    capability_abis,
    layout_roots,
}
```

Public code-generation entry points accept `CheckedProgram`, not a bare AST
plus optional tables. Test-only constructors that intentionally forge evidence
are kept in a dedicated negative-test module and cannot be linked into release
code.

Every rewrite that creates a new expression also creates or derives its typed
judgment. A rewrite cannot rely on source-node pointer identity surviving. A
missing table entry, unresolved type variable, incomplete constructor result,
unknown callable signature, or conflicting layout is a hard internal compiler
error. There is no default `i32`, `Slot`, boxed, or generic fallback at a
supposedly typed boundary.

### 2. A memory-safe WIR dialect

Raw addresses are not the interface used by ordinary lowering or optimization.
WIR gains compiler-only value classes that carry no runtime payload:

```text
MemRef       { root, layout, lifetime, access }
Place        { base: MemRef, projection, layout }
ByteRange    { base: MemRef, start, length, proof }
OwnedValue   { place, ownership_state }
IndexProof   { index_domain, length_source, root_epoch }
SizeProof    { mathematical_size, wasm_size, no_wrap }
```

They are Rust types and verifier facts, not Wasm structs. A `MemRef` still
encodes as the same `i32` address used today. A `Place` adds no field to an
allocation. `IndexProof` and `SizeProof` are erased.

Ordinary WIR construction uses semantic operations:

```text
Alloc(layout, count, size_proof) -> MemRef
Project(base, field, expected_layout) -> Place
Index(base, index, index_proof, expected_layout) -> Place
Load(place) -> Value
Store(place, value)
Copy(src_range, dst_range, copy_kind)
Fill(dst_range, byte)
Retain(owner)
Release(owner)
Move(owner) -> OwnedValue
Borrow(owner, access, lifetime) -> MemRef
```

The operation owns the invariant. A field projection uses a canonical layout
descriptor rather than an arbitrary byte offset. An indexed projection carries
the exact length source, stride, and non-wrapping address proof. `Copy` states
whether it is a bitwise copy, an ownership-preserving clone, or a consuming
move; a reference-carrying layout cannot accidentally select bitwise copy.

Optimizers rewrite these semantic operations and their evidence. They do not
rewrite raw `i32.load` and `i32.store` trees.

### 3. The sealed memory kernel

Some code must ultimately implement allocation headers, RC operations, region
rewinds, and byte copies. Raw memory instructions therefore remain in one
sealed WIR memory-kernel module, not as general-purpose constructors.

Each kernel function has a machine-checkable contract over layouts, ranges,
ownership effects, traps, and results. A contract is a proof obligation, not a
trusted annotation: the memory verifier checks the raw kernel body against it.
Only the primitive semantics of Wasm memory, integer operations, and traps are
axioms at this layer. The kernel builder is the only code that can construct raw
memory nodes. Its API accepts the typed operands above and returns typed
results. Direct construction outside that module is prevented by Rust
visibility and a workspace structural test.

The initial kernel contracts cover:

- checked bump allocation and growth;
- RC allocation, retain, release, and free-list reuse;
- region allocation and rewind;
- typed fixed-layout clone and move;
- dynamic sequence allocation and resize;
- checked field and element projection;
- byte copy, fill, and string/bytes access; and
- compiler-generated suspension and closure storage that remains in linear
  memory.

Wasm GC operations retain their native typed references and bounds behavior.
They are checked for exact GC type indices and field kinds but do not pass
through linear-memory provenance.

### 4. Proof-producing analyses

Facts are issued by the stage that owns them and name their provenance.
Examples include `Typeck`, `CanonicalLayout`, `DominatingCheck`,
`ConstructorExtent`, `UniqueOwner`, `StableRoot`, and `KernelContract`.
Optimizers may refine facts but cannot mint stronger provenance from a boolean
or a function-name match.

The following rules are mandatory:

- Size arithmetic is modeled mathematically before conversion to Wasm widths.
  Every narrowing records a proven range.
- Branch facts apply only in dominated regions and are intersected at joins.
- Calls invalidate any range, uniqueness, or root-stability fact not preserved
  by the exact checked call effect.
- Unknown and indirect calls receive conservative effects unless their exact
  checked callable ABI and summary are available.
- A fresh-allocation fact comes only from a recognized allocation operation or
  a checked callee result contract. Source spelling is never evidence.
- Layout equality is exact `LayoutId` equality or a checked adapter. Similar
  shapes are not interchangeable.
- Ownership is a state transition, not a collection of independently mirrored
  retain and release predicates.

An optimization interface returns either a transformed operation with complete
evidence or `Declined(reason)`. `Declined` selects the safe baseline lowering.
It is not an error for normal Witchy source and cannot change observable
semantics.

### 5. Post-optimization WIR verification

`verify_memory_wir` runs after all Witchy WIR rewrites, helper insertion,
specialized-layout selection, and module assembly. It independently checks:

1. Wasm-level expression and callable kinds.
2. Allocation roots, extents, lifetimes, and region epochs.
3. Non-wrapping offset, stride, count, and byte-length arithmetic.
4. Exact layout identity for every projection and memory operation.
5. Ownership state across structured control flow, calls, returns, traps, and
   suspension points.
6. Borrow validity and root stability across writes and opaque calls.
7. Source and destination validity for every bulk operation.
8. Kernel call preconditions and postconditions.
9. Host import signatures and the impossibility of scalar-to-reference
   reinterpretation.
10. Completeness: every operation that can emit a Wasm memory instruction has
    one verified origin.

The verifier performs conservative abstract interpretation over WIR's existing
structured control flow. Joins retain only facts valid on every incoming edge.
Loops use a deterministic fixed point with widening; failure to establish an
invariant rejects the optimized form or, when still available, recompiles the
function through the safe baseline path.

Verification failure never produces best-effort Wasm. Diagnostics name the
function, source span where available, operation, missing fact, and fact
provenance. These are compiler defects, not user type errors.

### 6. Validation of the exact final bytes

Verifying WIR alone would still trust the encoder and any postprocessor. The
WIR verifier therefore emits sealed instruction obligations before encoding:

```text
MemoryInstructionWitness {
    function_index,
    instruction_ordinal,
    verified_operation_id,
    opcode,
    width,
    static_offset,
    address_expression,
}
```

The encoder supplies only instruction/source ordinals. It cannot create or
strengthen an obligation. The independent final-byte checker parses the exact
candidate module, reconstructs each opcode and address expression, and requires
a one-to-one match between every linear-memory instruction and a sealed WIR or
memory-kernel obligation. It also checks callable signatures, GC type indices,
imports, tables, and memory declarations. Missing, duplicate, reordered, or
changed instructions reject the module. An encoder defect that changes both an
instruction and its source map therefore still fails against the pre-encoding
obligation.

The witness exists only in compiler memory. It is neither trusted input nor a
production custom section. The compiler returns bytes only after ordinary Wasm
validation and the final-byte check both succeed.

A binary postprocessor such as `wasm-opt` invalidates the witness. Such output
may execute only if one of these conditions holds:

1. the postprocessor emits a new proof accepted by the final-byte checker;
2. the transformed bytes are imported into a verified low-level form and the
   complete memory proof is re-established; or
3. the postprocessor is disabled for that module.

Checking only the pre-optimization bytes is forbidden. Optimized-Wasm caches
key and retain the exact validation result for the exact byte digest and
optimizer identity; a cache mismatch returns to validation, never directly to
execution.

### 7. Zero runtime overhead contract

The safety architecture is compile-time only:

- proof values, fact lattices, witnesses, counters, and diagnostics are absent
  from production Wasm;
- the verifier cannot insert an instruction, branch, import, global, memory
  field, tag, canary, or redzone;
- release Wasm produced with verification enabled is byte-identical, excluding
  non-executable name data, to the candidate bytes that the verifier accepted;
- an optimization that needs a new dynamic guard is not promoted as a proven
  optimization; it declines to the safe baseline; and
- checks already required by Witchy semantics, such as bounds and allocation
  failure checks, remain part of the baseline and may be removed only by proof.

There is a hard limit worth stating directly: object bounds for an arbitrary
dynamic index cannot be guaranteed by Wasm linear-memory validation alone. The
compiler must either prove the index statically, use an operation with native
bounds semantics such as a Wasm GC array access, or retain the semantic bounds
check. This RFC adds no check on top of that choice and does not rename an
unsafe unchecked access as zero-cost.

Implementation acceptance includes instruction-count, code-size, allocation,
and matched timing controls. The verifier itself must have exactly zero dynamic
instruction delta. Representation repairs discovered during migration must
meet the protected performance gates below; they are not allowed to hide a
safety tax in an unmeasured path.

### 8. Compiler-process hardening

All compiler-owned crates on the source-to-Wasm path continue to use
`#![deny(unsafe_code)]`. CI adds a transitive unsafe-code census for the exact
features used by the compiler. Each allowed dependency entry records:

- why it is reachable on adversarial input;
- which unsafe operations it contains or encapsulates;
- its version and update policy;
- its fuzz, Miri, or upstream safety evidence; and
- whether a memory-safe replacement or process boundary is feasible.

Parsers and decoders consume explicit byte, item, recursion, and allocation
budgets. Compiler caches never deserialize unchecked native objects or trust
cached type/layout evidence; cached inputs re-enter through the same validation
boundary as fresh inputs.

This is an explicit trusted computing base, not a claim that `deny(unsafe_code)`
proves every dependency correct.

## Performance and artifact gates

Promotion requires all of the following on a pinned master baseline and quiet
machine:

1. For every accepted candidate, observing the verifier result does not mutate
   its code section: the bytes immediately before and after verification are
   identical. A rejected optimized candidate may be replaced only by a newly
   compiled safe-baseline candidate, which must pass verification in its own
   right.
2. No production module contains verifier imports, verifier globals, proof
   tables, safety tags, redzones, canaries, or sanitizer branches.
3. Whole-corpus executable Wasm size has zero increase attributable to the
   verifier. Name and development source-map sections are reported separately.
4. Deterministic dynamic-instruction counts for the protected kernel suite have
   zero increase attributable to verification.
5. Two warmups and twelve paired, interleaved samples show no protected
   workload regressing by more than 2 percent and no geomean regression.
6. Compiler time and peak memory are measured and reported. Compile-time cost
   may increase, but a greater than 10 percent corpus compile-time or peak-memory
   increase requires an accepted follow-up decision before promotion.

The protected suite includes allocation-heavy, RC-heavy, packed-layout,
string, list, dictionary, region, closure, async, and opt-mode reference
workloads. It also includes malicious near-miss programs that force every
optimization fallback.

## Adversarial verification

The verifier is tested primarily by attempting to make it accept bad modules.
The negative matrix includes:

- offset, multiplication, addition, and narrowing wraparound;
- negative or oversized dynamic capacities;
- one-past-object loads and stores that remain inside the Wasm linear memory;
- stale aliases after storage reuse;
- double release, release without retain, and retain of interior pointers;
- packed/default and cross-`LayoutId` confusion;
- incomplete constructor destination writes;
- byte copy with a valid source but short destination, and the converse;
- overlap claimed as non-overlap;
- call results falsely labeled fresh or unique;
- function-name and qualified-name lookalikes;
- facts surviving calls, writes, joins, loop back edges, suspension, or region
  rewind when they should be invalidated;
- forged closure, capability, GC, and host-call representations; and
- encoder mutation after WIR verification.

Mutation tests delete or invert each verifier rule and require at least one
negative fixture to fail. A rule without a killing fixture is not accepted.

Every SEC reproducer becomes three durable artifacts where applicable:

1. a source-level optimized-Wasm versus safe-baseline regression;
2. a forged-WIR negative test rejected for the named invariant; and
3. a final-byte mutation rejected at the exact offending instruction.

Sanitizers, redzones, poison-fill, no-reuse heaps, differential fuzzing, and
interpreter/optimized-Wasm/safe-Wasm comparison remain CI-only evidence. They
do not become production enforcement.

## Dependency graph and delivery

```text
checked lowering entry
        |
        +--> canonical layouts and typed call effects
        |             |
        |             v
        +------> memory-safe WIR dialect
                        |
        sealed memory kernel contracts
                        |
                        v
             post-optimization verifier
                        |
                        v
              proof-producing encoder
                        |
                        v
          exact final-byte witness checker
                        |
              +---------+---------+
              |                   |
       cache integration    postprocessor policy
              |                   |
              +---------+---------+
                        v
                 release promotion
```

Delivery is split so that proof producers land before optimization consumers:

1. **Inventory:** enumerate every raw memory constructor, allocator, layout
   adapter, ownership transition, compiler unsafe dependency, and binary
   postprocessor. Freeze the adversarial corpus and protected performance set.
2. **Checked entry:** make `CheckedProgram` the only production lowering input
   and remove type/ABI fallbacks.
3. **Typed memory operations:** introduce the erased WIR evidence types and
   migrate one operation family at a time without changing emitted bytes.
4. **Kernel closure:** seal raw operations, contract the allocator/RC/region
   kernel, and enforce the boundary structurally.
5. **WIR verifier:** run in report-only mode only while the inventory is
   incomplete, then fail closed. Report-only mode cannot support a release
   safety claim.
6. **Final-byte checker:** cover the encoder, helper insertion, Wasm GC types,
   imports, and tables.
7. **Postprocessor and cache closure:** re-prove exact executable bytes or
   disable the transform/cache path.
8. **Promotion:** complete the negative mutation matrix, SEC corpus, compiler
   TCB review, artifact equality, and performance gates.

Independent implementation tracks may work in parallel after the evidence
types, kernel contract, and witness schema are frozen. One integration track
owns the end-to-end fail-closed path.

## Acceptance ledger

No row may be marked proven by prose, backend agreement alone, or a green full
gate. Each requires the named executable artifact and exact merged revision.

| Contract | State | Required evidence |
|---|---|---|
| Compiler-process memory-safety TCB is explicit | pending | Unsafe dependency census, budgets, fuzz targets, Miri-applicable crates, pinned revisions |
| Only checked typed programs enter production lowering | pending | API visibility test plus forged/missing type, call, and layout rejection matrix |
| All ordinary memory operations use typed WIR | pending | Structural census with zero raw constructors outside the sealed kernel |
| Memory kernel contracts are complete | pending | Contract tests and mutation-killing fixtures for allocator, RC, regions, sequences, and copies |
| Post-optimization WIR is memory-safe | pending | Fail-closed verifier over the full corpus and every negative invariant family |
| Final executable bytes match the proof | pending | One-to-one final-byte witness tests, including encoder and postprocessor mutations |
| Optimized-Wasm and cache paths cannot bypass proof | pending | Exact-digest cache tests and postprocessor revalidation or disabled-path evidence |
| Every confirmed SEC reproducer is permanent | pending | Source, forged-WIR, and final-byte artifacts with named invariant classifications |
| Verification has zero runtime footprint | pending | Byte equality, zero artifact additions, dynamic instruction counts, allocations, and code-size report |
| Protected performance does not regress | pending | Pinned paired raw samples satisfying the performance gates |
| Release pipeline fails closed | pending | Focused shards, adversarial review, serialized full gate, and a deliberate invalid-module release test |

The RFC remains `planned` at most until every row is proven. `implemented`
requires the fail-closed release path, not merely the WIR types or verifier in
report-only mode.

## Alternatives

### Rely on standard Wasm validation

Rejected. Wasm validation cannot see Witchy allocation boundaries, layouts,
lifetimes, ownership, or intended host-call representations.

### Rely on tests, fuzzing, and differential execution

Rejected as the production rule. These remain essential evidence and find
errors in the verifier, but they cannot quantify over all programs or optimizer
interactions.

### Add runtime redzones, canaries, or dynamic ownership checks

Rejected. They add execution cost, detect only some violations, and often
report corruption after it occurred. They remain useful CI sanitizers.

### Disable all optimizations

Rejected. The safe baseline is necessary as a fallback, but Witchy's ownership,
layout, bounds, and loop optimizations remain important. Proof-carrying
transforms preserve them without speculative unsafety.

### Trust the WIR verifier and not inspect encoded bytes

Rejected. That leaves the encoder, helper insertion, binary optimizer, and
cache substitution outside the claim.

### Move every heap value to Wasm GC

Deferred as an independent representation option, not a complete answer. Wasm
GC removes classes of linear-memory errors and supplies native typed field and
array operations, but byte strings, regions, host buffers, and other linear
memory remain. Compiler type and ABI soundness still require verification.

### Formally verify the complete compiler now

Not required for the first cut. A small typed memory dialect, sealed kernel,
independent verifier, and exact-byte checker create a tractable trusted core
that can later receive machine-checked proofs without blocking immediate
fail-closed enforcement.

## Drawbacks

- Compile time and compiler memory use increase.
- The initial migration touches most code that constructs WIR memory operations.
- Conservative proof loss may temporarily decline valid optimizations.
- Final-byte verification constrains arbitrary binary postprocessing.
- The verifier and kernel become security-critical code and need unusually
  strong review discipline.
- A universal guarantee is only as strong as the named compiler, validator,
  encoder, dependency, Wasm engine, and hardware trusted computing bases. This
  RFC makes that boundary explicit rather than claiming mathematical certainty
  from tests.

## Prior art

- Typed intermediate representations make invalid transformations harder to
  express and localize proof obligations.
- Translation validation checks the output of an optimizer rather than trusting
  the optimizer implementation itself.
- Proof-carrying code separates an untrusted producer from a smaller checker.
- Rust's ownership model, borrow checking, and unsafe-code boundaries inform
  the separation between ordinary WIR and the sealed memory kernel.
- CompCert demonstrates the value of preserving semantics across compiler
  passes, while this RFC selects a smaller translation-validation boundary for
  Witchy's current compiler.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
-->
