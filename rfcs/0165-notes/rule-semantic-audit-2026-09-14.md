# RFC-0165 rule semantic audit, 2026-09-14

## Scope and result

Three independent read-only reviews compared each canonical rule's checker judgment, interpreter behavior, backend mapping, and manifest claim. The core census comprised 43 operation and terminator identities. The manifest's separately tracked closure identities, `ClosureSlot` and `ClosureOwner`, were also included in the ownership/closures review, so the review covers every row in the current 45-row generated manifest.

The reviews found the concrete mismatches listed below. Each has a committed disposition on `review/rfc0165-memory-rules`. No other concrete semantic mismatch was found in the reviewed rule mappings.

This review supports RFC-0165 B1 and B7 only after the branch reaches a terminal merged event. It does not establish compiler-wide memory safety, source-language type soundness, or general optimizer correctness.

## Rule census

### Scalar, control, and calls

`ConstUnit`, `ConstInt`, `ConstFloat`, `ConstBool`, `ConstSize`, `CapabilityNone`, `CapabilitySome`, `CapabilityIsPresent`, `CapabilityUnwrap`, `Arithmetic`, `Compare`, `NarrowSize`, `WidenSize`, `FloatUnary`, `Guard`, `FunctionValue`, `Call`, `VarCall`, `HostCall`, `Branch`, `BranchIf`, `Return`, `TailCall`, `VarTailCall`, `Trap`, and `Unreachable`.

### Ownership and closures

Core census: `MoveValue`, `MakeInlineAggregate`, `ExtractInlineField`, `CloneRef`, `Retain`, `Release`, `DropRef`, and `Reclaim`.

Separately tracked closure rows: `ClosureSlot` and `ClosureOwner`, including their authenticated layout/signature bindings and nested owner-operation census.

### Memory and layout

`Alloc`, `StaticData`, `Load`, `InitNullableOwned`, `Store`, `CopyBytes`, `MoveRange`, `CloneRange`, and `UpdateCount`.

## Findings and committed dispositions

- Reachable `Unreachable`: the checker admitted `Unreachable` on a reachable path even though the manifest described only dead blocks. Commit `c4b010cba` rejects it during reachable-block traversal and preserves dead-block use for pruning.
- Guard domain: `Arithmetic` guards could enter the range-fact domain without the unsigned `Size` restriction applied to `Bounds`, while `DynamicType` equality is legitimately used for closure tags. Commit `c4b010cba` limits range-producing `Bounds` and `Arithmetic` guards to unsigned `Size` comparisons and documents `DynamicType` separately.
- Explicit `Release` plan: the backend plan omitted the mapping for an admitted explicit shared release. Commit `d10bc31c6` adds the drop-reference plan and strengthens the admitted evidence as `shared_release_preserves_root_for_use`.
- Drop-frame failure atomicity: recursive drop cleared the parent's owned edge before worklist growth could trap. Commit `628a96afa` reserves a child frame before transferring ownership, preserving the parent edge and preventing partial reclamation on growth failure.
- Allocator 4-GiB boundary: allocation arithmetic could truncate an exclusive end beyond the Wasm address space before trapping. Commit `e5caa87cb` checks the widened allocation end before mutation; commit `e1faaa75a` records the exact representable boundary and runtime trap evidence.
- `Tag8`/`Tag16` truncation: the checker could authenticate values the backend would narrow silently. Commit `98662ca78` requires the exact representable ranges before initialization is recorded; commit `e1faaa75a` supplies backend boundary and trap evidence.
- Same-object `CopyBytes`: the interpreter reused ownership-sensitive range-copy semantics and refused byte overlap that the checked byte operation permits. Commit `39f0fbc77` gives `CopyBytes` snapshot/memmove semantics for forward overlap, backward overlap, and self-copy while leaving same-object `MoveRange` and `CloneRange` refused.
- Bump/runtime-buffer admission mismatch: checker admission exceeded backend mappings for `Bump` allocation and non-raw runtime-buffer access/range operations. Commit `98662ca78` rejects those unsupported mappings before they can mint authority.
- Pointer `CloneRange` publication ordering: destination pointers could be published before every child retain had succeeded, exposing partial ownership state after a saturated-count trap. Commit `e5caa87cb` retains the entire source prefix before publishing the destination; commit `e1faaa75a` proves a later saturated child leaves the destination unpublished.
- `CopyBytes` predicate: the manifest incorrectly required both slices within logical length and did not state publication ordering. Commit `e491a9bfc` records the actual judgment: initialized source within logical length, unique destination within capacity, and initialized copied bytes unpublished until checked `UpdateCount` advances logical length. It also records interpreter memmove semantics.

## Evidence boundary

The generated manifest remains the mechanical rule-to-test crosswalk. `manifest_document_is_current` passed with the established bless path after the predicate correction, and it verified that the strengthened Release evidence name still resolves. The review is evidence about agreement among the stated checker, interpreter, backend, and manifest semantics for these rule identities; its security significance depends on terminal merge and the broader RFC-0165 acceptance boundary.
