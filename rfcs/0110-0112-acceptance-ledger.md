# RFC-0110 / RFC-0111 / RFC-0112 acceptance ledger

Audit target: integration commit `4bdebf5cb4a9f5d3765a4b08351bf3ef67a2bbe1`
(the queued RFC-0110/0111/0112 batch), pending its serialized gate landing.

This ledger is the completion authority for the Rust-class `mode opt` program.
An RFC reaches `implemented` only when every row below is **PROVEN** by
current-master executable or structural evidence. The intermediate `accepted`
status records that the design decision is ratified and the remaining work is
tracked (not that it is finished): RFC-0110, RFC-0111, and RFC-0112 are all
`accepted` as of 2026-08-03 with the open rows below carried in each RFC's
`tracking:` field. `proposed` is not used as a resting state — an unratified
design stays a draft; a ratified one is `accepted`.

Statuses:

- **PROVEN** — current-master evidence directly covers the complete criterion.
- **PARTIAL** — a predecessor mechanism exists, but at least one named boundary
  or evidence leg is absent.
- **FAILING** — current behavior directly contradicts the criterion.
- **MISSING** — the required implementation or durable evidence does not exist.

## Dependency graph and track ownership

```text
RFC-0110 access signature + verifier
        │
        ├── physical ownership envelope ──┬── RFC-0111 cross-boundary layouts
        │                                 └── RFC-0112 callable owner relations
        │
        ├── general unique/place facts ────── RFC-0111 destinations + headers
        └── projection-aware loan facts ───── RFC-0112 aggregates + containers

RFC-0111 canonical descriptor ─────────────── RFC-0112 borrowed containers
```

The integration track owns this ledger, shared conformance/parity fixtures,
spec/book changes, RFC status transitions, and contract-drift resolution. The
initial independent tracks own:

| Track | Frozen interface / initial files |
| --- | --- |
| RFC-0110 access foundation | `witchy-types::access`; no syntax, WIR, or lowering edits |
| RFC-0111 layout foundation | `witchy-wir::layout`; no syntax, typeck, lowering, or runtime edits |
| RFC-0112 syntax foundation | nominal lifetime parsing/formatting/kinds; no loans, WIR, lowering, or runtime edits |

After the two foundation interfaces merge, implementation branches must consume
them instead of creating parallel AST-shape or operation-name catalogs.

## RFC-0110 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **PROVEN** | `tests/rfc0110.rs::every_access_consumer_uses_the_checked_logical_envelope` covers direct, method, trait, function-value, lambda, Apply, existential, and tail-dispatch consumers; the checked signature is shared by lowering and reflection. |
| 2 | **PROVEN** | Opt-mode analysis checks BOTH `var unique` and `own unique` call shapes at every call site (`CallOwnershipFact::unique_params_to_check` unions the write-back and consuming axes; `own_unique_consuming_param_is_checked_and_fresh_values_pass` proves opt-rejects-aliased / normal-accepts / fresh-owner-passes), and normal mode repairs by one copy — the aliased owner takes the zero-token copy-on-write boundary (value-equal to the interpreter's `Rc::make_mut`), counted deterministically by `boundary_reown_counter_is_exact_runtime_and_lever_invariant` (aliased = 1, fresh = 0, dead branch = 0, identical under all/none/-inplace/-unbox). |
| 3 | **PROVEN** | The RFC-0110 access matrix checks direct, function-value, lambda, existential-witness, and indirect calls with value/capacity result envelopes and exact destination order. |
| 4 | **PROVEN** | Callable-envelope reflection and `combined_access_envelope_cannot_be_erased_at_any_ascription_boundary` reject convention, qualifier, ownership-output, and lifetime erasure across the supported callable shapes. |
| 5 | **PROVEN** | Canonical checked-place facts and the fixed-place overlap matrix cover nested fields, fixed indices, dynamic-index fail-closed behavior, and all source call entrypoints. |
| 6 | **PROVEN** | Move-in/write-back semantics ship, and the general direct-storage `var` lowering now lands under the `direct-storage-var` lever (`Opt::DirectStorageVar`). When every write-back place is a whole local (`CodegenPlace::Root`, the six ownership proofs upheld — P1/P6 by the whole-local shape, P2–P5 by the checked var-unique write-back this arm is only reached for), the reconstruction commits `SetLocal root = result` directly instead of round-tripping through a `root_scratch` local. `tests/rfc0110.rs::direct_storage_var_writeback_is_counted_and_de_opt_equivalent` proves the lever-on counter fires, the value equals the interpreter oracle, and lever-off is byte-for-byte the de-opt reconstruction (counter 0, same value); the 732-case differential example suite confirms backend parity. |
| 7 | **PROVEN** | The access matrix asserts self-tail and mutual-tail dispatcher WIR shape, ordered value/capacity write-back, and absence of recursive calls in the lowered loop. |
| 8 | **PROVEN** | `tests/rfc0110.rs` runs direct, function-value, lambda, existential-witness, fixed-place, and self-tail cases through the interpreter, optimized Wasm, full de-opt, every single-lever de-opt, and an independent oracle; the paired normal-repair case (`boundary_reown_counter_is_exact_runtime_and_lever_invariant`) runs the aliased-owner repair value-equal across interpreter and every lever and asserts the exact repair count. |
| 9 | **PROVEN** | All three ownership counters are now real and lever-invariant: boundary re-own and ownership-token repair (`__witchy_boundary_reown_copies` / `__witchy_ownership_token_repairs`, proven exact = 1 repaired / 0 accepted / 0 dead-branch), and the direct-storage-access counter (`__witchy_direct_storage_var_accesses`), which the direct-storage `var` lowering fires exactly once per whole-local write-back (proven = 1 under the `direct-storage-var` lever, 0 with the lever off or absent, by `direct_storage_var_writeback_is_counted_and_de_opt_equivalent`). The tail-call recognizer (`wir_opt/tail_calls.rs`) treats the counter `SetGlobal` transparently so the ownership-envelope tail loop still folds. Indirect ownership-envelope calls and destination forwarding also have real counters. |
| 10 | **PROVEN** | `spec/performance.md` documents the shipped typed callable envelope, the normal-mode operation-level copy-on-write repair, the direct-storage `var` lowering, and all three ownership counters (`boundary_reown_copies`, `ownership_token_repairs`, `direct_storage_var_accesses`) as `witchy stats` fields. Every documented counter is a real `Stats` field (`src/stats.rs`) fed by a live `Vm` reader, and every claim is backed by a passing `tests/rfc0110.rs` proof. |

## RFC-0111 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **PROVEN** | `witchy-wir::layout` provides versioned canonical `LayoutId` descriptors, deterministic encoding/digests, interning, and artifact bundle validation; WIR and lowering consume the same interner. |
| 2 | **PROVEN** | Packed records/lists cross direct, stored, linked, parameter, return, and field boundaries in `codegen_tests::declared_packed_values_cross_direct_and_stored_boundaries` and linked fixtures. |
| 3 | **PROVEN** | Generic packed helpers are physically specialized by logical/access/layout/optimization identity, with construction, traversal, mutation, and return tests. |
| 4 | **PROVEN** | Callable-layout and access-envelope tests cover direct functions, function values, closures, traits, and existential witnesses with exact descriptor identity. |
| 5 | **PROVEN** | Artifact layout sections and generated host-import metadata authenticate schema, canonical bytes, child descriptors, accepted IDs, counted-marshal targets, and reject-all fallbacks before adapter selection. |
| 6 | **PROVEN** | Destination-forwarding tests assert exact dead destinations, zero intermediate packed allocations, counters, and conservative fallback for incomplete/escaping results. |
| 7 | **PROVEN** | `codegen::header_elision::proven_header_free_lists` performs whole-graph closed-domain selection. `codegen_tests::proven_unique_packed_list_elides_exactly_one_rc_header` compares header-free and RC-backed values, physical bytes, allocation counters, and de-opt levers; `header_elision_falls_back_at_ownership_and_domain_boundaries` proves conservative rejection at sharing boundaries. |
| 8 | **PROVEN** | WIR layout and lowering tests cover fixed-tag/aligned closed sums, descriptor-driven equality, drop/copy shapes, destination forwarding, and loud rejection of variable-layout payloads. |
| 9 | **PROVEN** | The reviewed report from the pinned ARM reference machine (Apple M4, arm64) is committed at `bench/rust-class/reports/arm64-reference.json`: all eight cases within the 1.50x per-case cap and geomean 0.92x within the 1.25x cap, scalar-verifier `verified`, commit/binary provenance and independent result oracle embedded. The timing gate is activated — `scripts/check.sh` `rust_class_check` runs `bench/rust-class/run.sh --verify-report` on the committed report every gate, re-checking the versioned shape, provenance, oracle, scalar certification, and both thresholds. |
| 10 | **PROVEN** | `example_tests::rfc0111_layout::cross_lever_specialized_layout_slice_is_green_on_every_lever_and_backend` runs one program covering packed records, a packed `List(P)`, a fixed-layout closed sum, and packed-field reads under the interpreter oracle, compiled Wasm parity, the runtime's always-on checked heap (trailing-redzone/UAF sweep in `Runtime::run`), and the full cross-lever de-opt sweep (`none`, `all`, and every single lever toggled off), all reproducing the exact value. Canonical descriptor artifact/bundle compatibility is covered by the `witchy-wir::layout` encode/decode/import tests (criterion 5). |
| 11 | **PROVEN** | Architecture and performance specs now describe canonical descriptors, packed cross-boundary support, destination passing, and the explicit fail-closed unsupported-boundary policy. |

## RFC-0112 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **PROVEN** | Nominal lifetime parameters parse, retain declaration order, participate in kind checking, and are reflected by the fixed borrowed nominal tests. |
| 2 | **PROVEN** | Fixed borrowed nominal records/tuples construct, copy, project, return, and preserve owner roots through linked/generic boundaries; erasing calls remain rejected. |
| 3 | **PROVEN** | Projection-aware loan facts preserve root/projection identity and reject relabeling or relation-erasing persistence while accepting exact owner-preserving shells. |
| 4 | **PROVEN** | `LoanOwnerRoot`, `LoanPlace`, `LoanProjection`, `LoanRootCompanion`, and `LoanEvent` preserve root/projection identity and fixed ranges. `loans_tests::persisted_projection_keeps_the_original_root_and_fixed_path`, `any_live_projection_blocks_mutation_of_its_owner_root`, and `fixed_ranges_are_facts_and_dynamic_projections_do_not_persist` cover persistence, overlap, and dynamic-index rejection across the checked facts. |
| 5 | **PROVEN** | Function-value lifetime relations, nominal owner positions, and the unified access signature are checked together in callable and fixed-nominal matrices. |
| 6 | **MISSING** | Borrowed aggregate shell mutation, field replacement loan sequencing, and root-set write-back transport do not exist. |
| 7 | **PARTIAL** | RFC-0083 rejects many temporary, dynamic, task/channel, closure, and ownership escapes. Aggregate-specific diagnostics and typed owned-companion materialization are absent. |
| 8 | **MISSING** | Aggregate root retain/drop balance has no early-return/`?`/branch/loop/poison/UAF matrix. |
| 9 | **MISSING** | `List(B('a))` ownership-root construction, traversal, copy, overwrite, drop, and erasure rejection do not exist. |
| 10 | **MISSING** | No runnable zero-copy parser and borrowed iterator exercise both backends with zero-materialization counters. |
| 11 | **MISSING** | The language/performance specs, reflection, generated docs, and book do not state a shipped borrowed-aggregate contract. |

## Required closeout evidence

Completion requires all 32 rows to be **PROVEN** on the exact master commit that
marks the RFCs implemented. At minimum, the checked evidence set contains:

1. focused parser/type/access/layout/loan/WIR/lowering/runtime tests;
2. independent semantic-conformance expectations plus interpreter/Wasm parity;
3. `WITCHY_OPT=none` and every new single-lever differential run;
4. deterministic allocation/copy/RC/layout/destination/root counters;
5. checked-heap, redzone, UAF, artifact-schema, host, worker, closure, trait,
   generic, tail, and place matrices;
6. the pinned scalar-only Witchy/Rust benchmark report and thresholds;
7. runnable book examples and current generated manifests/docs;
8. `./scripts/test-for-paths.sh --run`-selected focused shards on each branch;
9. a green serialized merge-queue gate for every landed commit; and
10. a final requirement-by-requirement audit against current `master`.

Passing a narrower predecessor test, compiling an interface, or landing a
foundation stage changes a row to **PARTIAL** at most; it is not completion.

## Remaining-work triage (2026-08-04)

RFC-0111 and RFC-0110 are now **implemented** (all rows PROVEN). RFC-0112 is
**in-sandbox but deep** — no external-evidence blocker, but each remaining row is
multi-session compiler work over a large surface, not a bounded slice:

- **RFC-0110** (all ten rows PROVEN): normal-mode **one-copy repair** (a `unique`
  parameter without a uniqueness proof takes one defensive operation-level
  copy-on-write in normal mode instead of an opt-mode rejection — rows 2/8/10) and
  general **direct-storage `var` lowering** with the six RFC proofs plus the three
  real ownership counters (`boundary_reown_copies` / `ownership_token_repairs` /
  `direct_storage_var_accesses`, rows 6/9). Landed across the uniqueness/access
  substrate (`crates/witchy-types/src/access.rs`, `crates/witchy-lower/src/analysis.rs`,
  `crates/witchy-lower/src/codegen/mod.rs`) and the tail-call transform
  (`crates/witchy-wir/src/wir_opt/tail_calls.rs`).
- **RFC-0112** (rows 6, 8, 9, 10, 11 MISSING): borrowed-aggregate shell mutation
  + field-replacement loan sequencing + root-set write-back (6), the aggregate
  retain/drop-balance matrix (8), `List(B('a))` lifecycle (9), a runnable
  zero-copy parser + borrowed iterator with zero-materialization counters (10),
  and the shipped-contract docs (11). The largest remaining track; mostly unbuilt
  runtime + codegen.

RFC-0112 is a candidate for the queue-sharded fan-out (RFC-0079) rather than a
single session; it is not externally blocked.
