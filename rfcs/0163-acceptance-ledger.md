# RFC-0163 implementation and acceptance ledger

This is the live implementation ledger for RFC-0163. The RFC acceptance table
is the contract. A row is complete only when its named production-boundary
evidence passes on current `master` and every supplying queue item has a
terminal `merged` event.

RFC-0162's object-memory judgment is a dependency, not a substitute for any row
in this ledger. RFC-0163 must consume the same final candidate identity that
RFC-0162 validates.

## Current-truth checkpoint

- Checkpoint: 2026-08-27.
- Proposal landed on `master` as `7ae0aa481`.
- Integration branch: `impl/rfc0162-0163-integration` in
  `.codex/worktrees/rfc0162-0163-integration`.
- RFC-0162 implementation is concurrently owned in separate branches. This
  track does not edit or replay those files; it consumes only landed SafeWIR
  interfaces.
- The dependency-bottom capability vocabulary is
  `crates/witchy-cap-model`. Capability operations are still catalogued in
  `crates/witchy-syntax/src/cap_ops.rs`, while low-level host imports are
  declared by lowering and registered by several runtime modules. There is no
  one-to-one canonical source-operation to final-import contract yet.
- Final WIR represents a host call as `WirExpr::CallHost { import, args }`.
  It carries no authority identity, use set, rights, scope, or origin.
- `safewir_validate::HostArgument` distinguishes scalar and checked buffer
  operands only. It has no authority provenance.
- There is no `CheckedAuthorityProgram`, `ValidatedAuthorityWir`,
  `AdmittedWirModule`, or `AdmittedExecutable` production type.
- There is no independent authority parser/checker and no Lean project or
  installed pinned Lean checker in the repository.
- Production encoding and launch have no RFC-0163 final-byte admission record.
  Binaryen remains a possible post-encoding transform in existing profiles.

## Frozen dependency graph

```text
T0 canonical capability, operation, import, target, and carrier contracts
 |                         |
 v                         v
T1 independent source     T2 landed SafeWIR object judgment
   authority judgment
 |                         |
 +------------+------------+
              v
T3 AuthorityWIR identities, contracts, and final flow validation
              |
              v
T4 one candidate identity and private combined WIR admission
 |                         |
 v                         v
T5 Lean judgment/checker  T6 exact final-byte checker
 +------------+------------+
              v
T7 no-postprocessor encoder and artifact admission path
 |                         |
 v                         v
T8 confined compiler      T9 cache, grant, import, and launch binding
   worker
 +------------+------------+
              v
T10 adversarial, zero-cost, performance, and release promotion evidence
```

T0 is the interface freeze. T1, T2, T5, T6, and T8 may proceed in parallel once
their canonical encodings are frozen. T4 is the integration critical path.
The merge queue serializes landing only.

## Shared contracts

The following contracts are dependency-bottom data. Syntax, type checking,
lowering, WIR validation, the final-byte checker, runtime import registration,
artifact admission, and launch policy consume them rather than maintaining
private name tables.

1. `AuthorityKindId` canonically identifies every built-in host, derived, and
   build capability.
2. `RightsSet` is a closed, normalized subset of the rights legal for its kind.
3. `AuthorityScope` is a canonical restriction value. It has an explicit
   unrestricted form and content-addressed normalized restricted forms; an
   unknown or unparseable scope is never unrestricted.
4. `HostOperationId` canonically identifies a source-visible operation and its
   one or more final host-import steps. Names are diagnostics, not identities.
5. `HostOperationContract` records receiver kind, carrier, required rights and
   scope relation, ordered authority uses, argument/result shapes, derivation
   or narrowing result, target availability, and final import signatures.
6. `AuthorityCarrier` distinguishes `ExternRef`, zero-representation roots,
   and no-authority operations. Scalar linear-memory values are never carriers.
7. `CandidateIdentity` is a digest of the canonical final function, definition,
   control, layout, import, object, and authority graph. SafeWIR and
   AuthorityWIR reports combine only when this identity is byte-for-byte equal.
8. `AdmissionSchemaId` versions the complete source, object, authority,
   encoder-template, and final-byte rule set. A schema mismatch rejects.

Every contract has canonical encode/decode round trips, duplicate-ID rejection,
unknown-enum rejection, explicit resource limits, and a stable digest fixture.

## Tracks

| Track | State | Ownership | Required output |
|---|---|---|---|
| T0 contract census and canonical schema | in progress | `crates/witchy-cap-model`, this ledger | Complete source operation, final import, right, carrier, derivation, and target census with no private runtime/lowering rows |
| T1 independent source authority | pending | new dependency-bottom checker crate/module | Independently parsed bounded source bundle and canonical `CheckedAuthorityProgram` |
| T2 SafeWIR dependency | concurrent | separate RFC-0162 tracks | Private, mandatory final object-safety admission over exact candidate identity |
| T3 AuthorityWIR | pending | new `witchy-wir` authority modules after T0 | Ghost identities plus validator-owned origins, transfer, join, call, capture, narrowing, derivation, and use fixpoints |
| T4 combined admission | pending | integration | Private wrapper requiring exact T1/T2/T3 candidate agreement; sole production encoder entry |
| T5 Lean model and checker | pending | new `proofs/rfc0163` tree and invocation shim | Pinned theorem plus executable checker or independently complete certificate checker |
| T6 exact final bytes | pending | new final-byte admission module | Standard Wasm validation plus one-to-one security-relevant instruction obligations |
| T7 postprocessor closure | pending | encoder/runtime integration | `postprocessor: None`; no transformed bytes reuse admission; protected performance recovered before promotion |
| T8 compiler confinement | pending | confinement/runtime/compiler worker | Capability-empty bounded worker and feature-resolved unsafe dependency census |
| T9 artifact/runtime binding | pending | artifact/cache/launch integration | Exact bytes, schema, checker, toolchain, catalog, target, grants, imports, and confinement binding |
| T10 promotion evidence | pending | tests/docs/release | Rule mutations, SEC-080/081, worker escape probes, zero-runtime-cost and protected performance evidence |

## Acceptance ledger

`pending` means current production code contradicts the row or the required
evidence is missing. `in progress` means a supplying contract is actively being
implemented but does not yet satisfy the row.

| # | Contract | State | Current evidence or contradiction | Evidence required to close |
|---:|---|---|---|---|
| 1 | Capability and host-operation census is complete | in progress | Capability kinds and rights are centralized, but capability operations and final host imports are separate catalogs | Every RFC-0126 capability, zero-representation authority, right, derivation, target, and import maps to one canonical contract; structural test rejects uncatalogued rows |
| 2 | Authority source judgment is independent | pending | Existing footprint/type checks consume the ordinary AST and type checker | Independent exact-source parse rejects typed-annotation, effect, generic, capture, dynamic, async, and operation-ID mutations |
| 3 | No-capability function has no authority | pending | Module imports and `CallHost` names do not encode per-function authority | `String -> String` dependencies reject every direct, indirect, specialized, inlined, closure, and async host-use injection |
| 4 | Narrow capability cannot widen | pending | Source typing has rights checks; no independent final authority flow exists | File-only and refined Dir, Net, Secret, derived, alias, aggregate, call, and return matrices prove monotonicity |
| 5 | Zero-representation capabilities have provenance | pending | Zero-representation calls have no final-WIR receiver lineage | Every such host call requires a root, parameter, or catalogued derivation origin before receiver erasure |
| 6 | Final WIR authority is rederived | pending | No AuthorityWIR validator exists | Optimizer metadata, stale identity, join, recursion, indirect-call, and call-summary mutations reject |
| 7 | SafeWIR and AuthorityWIR bind the same candidate | pending | No shared candidate identity exists | Function, definition, control, layout, import, object, and authority graph mismatches cannot combine |
| 8 | Production has one private WIR admission gate | pending | Ordinary WIR encoding paths remain while RFC-0162 migrates | Encoder, artifact, cache, debug, raw-body, and fallback negative API/compile-fail tests accept only combined admission |
| 9 | Lean model states and proves the scoped claim | pending | No formal model exists | Pinned theorem proves object-safety composition, authorized host traces, and no authority amplification |
| 10 | Shipped admission uses the proved checker | pending | No checker invocation exists | Every claimed artifact runs the pinned Lean-derived checker or an independently complete certificate checker |
| 11 | Exact final bytes are checked | pending | Only ordinary Wasm validation is mandatory | Every memory, reference, kernel, host, signature, target, and security-trap mutation rejects after standard validation |
| 12 | No unvalidated postprocessor remains | pending | Existing production profiles may invoke Binaryen | Claimed artifact records `postprocessor: None`; changed bytes require complete readmission |
| 13 | Protected performance is retained | pending | No matched admitted-profile measurements exist | Required former Binaryen wins are implemented before admission or satisfy approved no-regression gates |
| 14 | Compiler-owned unsafe surface is bounded | pending | No profile-specific feature-resolved census is bound to release evidence | `deny(unsafe_code)`, dependency exceptions, owners, budgets, evidence, and fail-closed CI are complete |
| 15 | Adversarial compilation is confined | pending | Existing runtime confinement does not create an RFC-0163 source worker | Capability-empty worker and malicious access/escape probes pass on every claimed platform |
| 16 | Artifact, cache, and launch binding is exact | pending | Existing artifact records lack the complete admission schema | Digest, schema, checker, toolchain, catalog, target, import, and grant mismatches reject or re-enter admission |
| 17 | SEC-080 and SEC-081 are complete corpus members | pending | Source regressions exist; authority and final-byte mutation layers do not | Source, forged-WIR, authority, final-byte, and end-to-end exploit tests all land |
| 18 | Generated execution has zero safety overhead | pending | No matched authority-enabled artifact comparison exists | Exact code bytes, instructions, imports, globals, memory, tables, allocations, code size, and protected benchmark evidence |
| 19 | Confined claim fails closed | pending | `--confine=required` exists independently of admission | Partial or unavailable providers reject and consume the same exact root-grant contract |

## Evidence rules

- A label, source span, optimizer fact, import name, typed annotation, or Rust
  Boolean is an untrusted claim. The admitting checker independently derives
  its obligation.
- A test closes a row only when its negative case reaches the production
  boundary named by that row.
- Rule coverage includes one accepted fixture, one rejected fixture, and one
  killed mutation for every stable rule ID.
- Exact final-byte tests mutate the bytes and any untrusted instruction map
  together. Updating the search aid cannot authorize changed bytes.
- Zero-cost comparisons fix identical optimization decisions and compare guest
  code bytes and representation. Artifact metadata is excluded only when it is
  never guest-mapped or executed.
- Performance evidence uses the protected whole-workload corpus, raw samples,
  tool identities, and current source SHA. Compiler-time cost is reported
  separately from guest execution.
- Queue state, raw counts, tool versions, and benchmark measurements are
  refreshed immediately before a completion claim.

## Closeout checklist

- [ ] Every acceptance row is `merged` with current production-boundary evidence.
- [ ] RFC-0162's completed object judgment and RFC-0163's authority judgment
      consume the exact same candidate identity.
- [ ] Every runtime and lowering host import is structurally tied to one
      canonical contract.
- [ ] Every stable Rust and Lean rule has accepted, rejected, and mutation-
      killing evidence.
- [ ] Exact final bytes pass standard Wasm validation and the proved admission
      checker, with no postprocessor afterward.
- [ ] SEC-080/081 and the no-capability/file-only scenarios pass end to end.
- [ ] Compiler-worker escape probes and `--confine=required` platform matrices
      pass fail closed.
- [ ] Zero-runtime-cost and protected performance gates pass.
- [ ] Every supplying queue item has a terminal `merged` event and its landed
      master SHA is recorded.
- [ ] RFC status changes only after this requirement-by-requirement audit.
