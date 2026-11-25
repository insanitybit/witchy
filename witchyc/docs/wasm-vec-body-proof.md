# Conditional generic record body proofs

This conditional body slice passed independent review and focused Rust/Lean
qualification at 727d788. It proves an unused generic source body and adds no
emitted-instance, executing-caller, body-byte-erasure or completed Vec claim.
Caller-state verification remains required for every substituted requires. The
subsequent [bounded caller slice](wasm-vec-caller-proof.md) establishes actual
allocation-derived prefixes and executes four layouts; this page describes the
conditional body theorem. Default ordinary Vec/String source remains unannotated.

The mode accepts one CopyStorage parameter T; one owned record parameter containing
one Buffer<T> through the existing owner spine; U64/T remaining fields and parameters;
at least one T payload parameter; and Result arms containing corresponding owner
records. It uses semantic field ordinals, independent of source record, field and
function names. Arrays, other numeric types, other owners/capabilities/references,
loops, runtime calls, allocation/release and other storage operations are outside A.

An ordinary generic predicate `memory.prefix(view,n,c)` means actual capacity=c,
n<=c, all indices below n Full, and all indices at/above n Empty. Public U64 header
fields are independent bounded inputs; no prefix or header-capacity relation follows
from their record type. Requires is optional. Removing prefix while retaining a
strong append postcondition must fail the kernel; removing it with ensures:true
can still prove the actual body's guarded Add safety.

Clauses support typed parameter/result record paths, old(parameter.field) entry
snapshots, capacity/Empty/cell/same-allocation/unchanged/unchanged-except/frame,
U64 math/comparisons/logic and logical generic-T equality/inequality. Requires reads
entry paths; ensures uses old snapshots for consumed record parameters, immutable
Copy parameters and actual active Result-arm binders. Binders cannot shadow any
parameter/result, escape their arm, capture runtime lets or create nested matches.
Requires plus actual post-predicate leaves across both arms share a32 limit; params32,
depth64, nodes4096 and generated statement/proof64KiB guards remain per definition.

The native append contract describes Ok(output) with same allocation, old length+1,
same saved capacity, new prefix, exact item in the old-length cell, other cells and
unrelated heap frame unchanged. Err(back) preserves actual returned vector headers,
owner/cells and exact item, and requires old length=old capacity. These ordinary
record paths describe the actual source library's unchanged push-spare body; the
compiler recognizes no Vec names or algorithm pattern.

One symbolic value representation retains logical scalars, record ordinals, active
Result payloads and the single existing Buffer focus. The bounded interpreter reads
the primary typed body once: bindings, moves/extracts, constructors, Copy reads and
field stores, finite If/Match, early returns, pure conditions, capacity, Init and
checked Add. Source Flow remains the sole move/loan/MUST/availability authority.
Complete RHS values are captured before logical stores. Short-circuit RHS execution
is guarded before collecting its Add checks. Init payloads initially require a
nonpointer field-only generic Copy read: constructor/project payloads with hidden
Add/effects refuse before evaluation. The qualified Init transition renderer is
shared; actual bounds/Empty/Full cases determine returned variant and after-cells.

Shared named ghost lets represent computations. Pending forks, recursive model work,
rendered bytes and expanded term/case costs are bounded before growth. Each executed
Add contributes its own guarded nonoverflow conjunct at its evaluation point. Neither
its own range, future output range nor authored postconditions become premises.
Unreachable Add checks remain guarded, rather than forcing an unconditional theorem.
Actual returned records/variants feed postconditions; no fresh unconstrained owner or
assumed output is substituted. Entry values and before snapshots survive source moves
only as ghost terms, never runtime owner copies.

One theorem quantifies arbitrary alpha, complete opaque allocation key, heterogeneous
heap frame, capacity/cells and independent record fields/payloads. Full-cell case
witnesses are quantified and tied to actual cells by path equality, not inactive
runtime carriers. Prefix facts are instantiated at relevant indices; inline index/
segment cases, equality/reduction and existing inline core Omega discharge obligations.
Original branch equations reduce transitions before broad simp_all. No representative
layout, DecidableEq/Inhabited alpha, auxiliary unselected theorem or new axiom is used.
Concrete result ABI limits remain mandatory for actual instances (7+T carriers for
the append wrapper, at most25), independently of storage's30 limit.

Body receipt source_model is `witchy.checked-storage-record.v1`, generic_parameters=1,
goal_kind=function_body and function=actual definition. Record identity is distinct
from Init; existing ghost/scalar/Init metadata/model text is preserved when absent.
The same expected-theorem/fresh source/compiler/model/tool/module checks, strict audit,
trust=0, allowlist, admission_reuse=false and atomic publication remain mandatory.

Kernel success establishes the generated abstract normal-return proposition under
only declared requires and valid typed entry state. Typed translation/VC completeness,
physical allocation identity/layout/allocator/bitmap/count/codec/emission remain TCB.
No physical stack/heap byte refinement or termination theorem is claimed. The
bounded caller slice interprets actual TryAlloc success/failure and constructors,
proves actual prefixes, and checks four-layout execution, proof erasure, returned
owners/items and unrelated-layout runtime sentinels. These tests do not turn the
physical primitive correspondence into a kernel theorem.
Full-library growth/loops, owner T, ZST and reusable admission remain open.
