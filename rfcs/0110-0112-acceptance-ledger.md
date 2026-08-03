# RFC-0110 / RFC-0111 / RFC-0112 acceptance ledger

Audit target: integration commit `4bdebf5cb4a9f5d3765a4b08351bf3ef67a2bbe1`
(the queued RFC-0110/0111/0112 batch), pending its serialized gate landing.

This ledger is the completion authority for the Rust-class `mode opt` program.
An RFC remains `proposed` while any row is not **PROVEN** by current-master
executable or structural evidence.

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
| 2 | **PARTIAL** | Opt-mode analysis enforces supported `var unique` / `own unique` call shapes, but general source-boundary enforcement and the required normal-mode one-copy repair are absent. |
| 3 | **PROVEN** | The RFC-0110 access matrix checks direct, function-value, lambda, existential-witness, and indirect calls with value/capacity result envelopes and exact destination order. |
| 4 | **PROVEN** | Callable-envelope reflection and `combined_access_envelope_cannot_be_erased_at_any_ascription_boundary` reject convention, qualifier, ownership-output, and lifetime erasure across the supported callable shapes. |
| 5 | **PROVEN** | Canonical checked-place facts and the fixed-place overlap matrix cover nested fields, fixed indices, dynamic-index fail-closed behavior, and all source call entrypoints. |
| 6 | **PARTIAL** | Move-in/write-back semantics and selected in-place self-assignment paths ship. A general direct-storage `var` lowering with the six RFC proofs does not. |
| 7 | **PROVEN** | The access matrix asserts self-tail and mutual-tail dispatcher WIR shape, ordered value/capacity write-back, and absence of recursive calls in the lowered loop. |
| 8 | **PARTIAL** | `tests/rfc0110.rs` runs direct, function-value, lambda, existential-witness, fixed-place, and self-tail cases through the interpreter, optimized Wasm, full de-opt, every single-lever de-opt, and an independent oracle. The paired normal-repair cases required by the RFC are absent. |
| 9 | **PARTIAL** | Indirect ownership-envelope calls and destination forwarding have real counters. Boundary re-own, ownership-token repair, and direct-storage access counters are still placeholders; `__witchy_reowns` measures operation-level copy-on-write instead. |
| 10 | **PARTIAL** | The language and performance specs, callable reflection, diagnostics, and book describe the shipped typed callable envelope. They cannot yet document the unimplemented normal repair, direct-storage lowering, or missing counter proofs as shipped. |

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
| 9 | **PARTIAL** | The paired corpus, independent results, cross-platform authenticated correctness gate, ARM scalar-instruction verifier, and ARM-only versioned report verifier exist. A reviewed report from a pinned ARM reference machine and activation of the 1.25x/1.50x timing gate remain absent. |
| 10 | **MISSING** | No cross-lever acceptance slice jointly covers specialized layouts under checked heap, redzones, UAF checks, parity, runnable docs, and artifact compatibility. |
| 11 | **PROVEN** | Architecture and performance specs now describe canonical descriptors, packed cross-boundary support, destination passing, and the explicit fail-closed unsupported-boundary policy. |

## RFC-0112 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **PROVEN** | Nominal lifetime parameters parse, retain declaration order, participate in kind checking, and are reflected by the fixed borrowed nominal tests. |
| 2 | **PROVEN** | Fixed borrowed nominal records/tuples construct, copy, project, return, and preserve owner roots through linked/generic boundaries; erasing calls remain rejected. |
| 3 | **PROVEN** | Projection-aware loan facts preserve root/projection identity and reject relabeling or relation-erasing persistence while accepting exact owner-preserving shells. |
| 4 | **PARTIAL** | Statement-identity last-use loans ship, including branches and loops, but facts lack a first-class projection/root/range representation. |
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
