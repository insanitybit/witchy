# Proof architecture and trust boundary

The active compiler emits core Wasm from a private primary typed body. Source
type, ownership, borrowing, capability and CopyStorage checks precede proof work;
Lean cannot grant a rejected source permission. Selected unused definitions and
unreachable source are checked, and their proofs are checked too, except those
of standard-library functions an executable build does not emit: those never
run, and the report names them (`unproved_unreached`; decisions.md, "Proving
only the library a program reaches", 2026-11-25). Wasmtime is the execution engine, not Lean or
a native compiler fallback. See [current support](wasm-cutover.md).

## Proof-required memory

The foundation includes generic `memory.get_unchecked(&Buffer<T>, index) -> T`
and `memory.init_unchecked(&mut Buffer<T>, index, item) -> ()`.
Authors supply Witchy requires/ensures and `proof: auto`. The compiler generates
bounds and nonempty-cell obligations from the actual typed operation and checks
its actual returned slot against authored postconditions. It models an opaque
generic element and actual initialized cell, without a default value or
concrete-type shortcut. Missing proof contexts and unsupported histories refuse.

[Typed named specification fragments](wasm-spec-predicates.md) let the library
reuse these expressions through one bounded expansion pass. They add no assumed
facts, runtime authority or separate predicate interpreter to the proof models.

The safe borrowed `vec_get` establishes physical bounds and initialization with
executable guards, also respecting logical length. Public length/capacity fields
are not an implicit invariant. The unchecked load omits the selected-index
bounds/bitmap guard; token/type/descriptor validation remains. Shared loans and
CopyStorage restrictions prevent ownership duplication. This readonly getter is
not an unchecked write/move, an arbitrary pointer API, full Vec safety or proof
of the growth/String algorithms. [Memory proof scope](wasm-proof-memory.md)
documents the exact supported finite subset.

## Kernel and compiler responsibilities

Each proof batch binds current source, compiler, model, audit and generated
module, with exact expected propositions, closed theorem inventory and allowed
axioms. Caller obligations cannot be replaced by a true unrelated lemma. Fresh
checking validates the complete SDK before and after the checker request.
Proof-free code resolves no checker installation.

The kernel establishes generated logical propositions. The source checker, VC
translation/completeness, modular summary dependency rule, logical-to-physical
allocation/bitmap mapping, element codec, address arithmetic and Wasm emission
remain trusted and tested. Wasm sandbox bounds do not prove per-object bounds or
initialization. The ordinary consuming Vec spare push now uses the raw initialization operation
inside its own one-owner body proof; success and failure statements describe the
actual returned vector and item. The ordinary reserve copy loop now has a general borrowed-buffer invariant
proof of copied-prefix/source preservation, including real early errors.
The actual reserve and cleanup bodies now have separate allocation/lifetime
proofs: real allocation error, fresh empty allocation, checked release and
current verified helper posts. The physical allocator and release/count mapping
remain trusted; these are normal-return resource/functional proofs.
The initialization slice proves bounds/Empty and the actual cell update,
fixed logical allocation/capacity, and preservation of other modeled cells.
`memory.frame_unchanged(old(buffer))` rendered only `frame = frame` and is now
refused. No contract states a frame theorem or proves physical heap isolation. The
physical footprint follows the trusted compiler/runtime implementation; it is
not established by that reflexive clause. Modular writes still havoc the
modeled cells, so a frame-only post cannot restore initialization facts.
The generic owned-record interpreter also checks subtraction before using its
natural-number result and models checked `memory.take` from the actual receiver
and index. Success returns the exact old nonempty cell and clears that cell;
bounds and empty-cell failures preserve the allocation and contents. The
[fixed-capacity Stack example](../examples/generic-proof-stack.wy) proves its
actual push/pop and cleanup bodies with these shared rules. Public headers are
checked rather than assumed valid. This is normal-return partial correctness
for CopyStorage elements, without a whole-container sequence or termination
theorem. Compound subtraction stores and unsupported effects still refuse.
Exclusive permission is checked by Source Flow; mutable modular calls use
after-argument snapshots and declared posts over havoced current cells.
Allocation remains fallible; mutation and cleanup retain their
supported checked semantics. No whole-compiler correctness, termination or
verified-immutable SDK claim follows from the kernel result.

The Wasm instance is the host-authority boundary. Mutually distrustful
components sharing one instance do not gain isolation from one another merely
through Wasm bounds or integer handles. Source ownership, capabilities and
object memory safety depend on the trusted compiler and runtime.

## Selected generations and publication

An explicit operator binding may select a body-interface helper and its SDK
generation. Full SDK checks occur around LOAD; the operator promises the private,
distinct-inode readonly SDK stays unchanged throughout the logical build session, including renewal
gaps.
Receipts state `verified_immutable:false` and `sdk_path_currentness:false`.
The binding selects that generation until session exit, not later changes to
its original SDK path. An invalid configured binding or failed kernel/protocol/
identity check refuses without switching checkers.

A selected helper may retain one audited generic body theorem; every current
application is kernel typechecked and all current caller/authored obligations
remain checked. This is not disk admission reuse. Healthy exhausted helpers
are stopped/reaped and replaced with empty theorem/query state before the next
request; renewal repeats startup validation and pins the original binding.
Failure does not trigger renewal as a retry. See [production lifecycle and
receipts](wasm-production-build.md).

Publication checks source snapshot/current receipt bindings and preserves the
previous output on failure. Artifact execution recompiles current packaged source
and compares emitted bytes and semantic identities before sandbox execution;
receipts are untrusted descriptions, not standalone admission certificates.

Historical native proof architecture is retrievable with
`git show 0731e426:docs/proof-architecture.md`. It does not define the active
compiler, cache policy or performance requirements.
