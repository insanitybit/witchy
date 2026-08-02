# RFC-0110 / RFC-0111 / RFC-0112 acceptance ledger

Audit baseline: `master` at `0ebee91d` on 2026-08-01.

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
| 1 | **PARTIAL** | Existing AST function types preserve conventions and borrow qualifiers, while `loans.rs` derives output-owner positions. No single checked access-signature value currently serves type checking, witnesses, closure tables, WIR, and tail lowering. |
| 2 | **PARTIAL** | `crates/witchy-lower/src/analysis.rs` checks selected no-copy contracts, but normal-mode one-copy repair and every call-shape source-facing enforcement are absent. |
| 3 | **FAILING** | `analysis.rs` explicitly reports that the first-class call ABI cannot carry a unique result ownership-capacity token. |
| 4 | **PARTIAL** | RFC-0083 callable checks reject several lifetime-erasing ascriptions, and convention type identity exists. One verifier does not yet cover conventions, uniqueness, write-back ownership output, and lifetime relations together. |
| 5 | **PARTIAL** | RFC-0087 place capture and overlap checks cover nested `var` places. The unique/access proof does not yet consume one canonical place/overlap result across every call shape. |
| 6 | **PARTIAL** | Move-in/write-back semantics and selected in-place self-assignment paths ship. A general direct-storage `var` lowering with the six RFC proofs does not. |
| 7 | **PARTIAL** | Proper-tail lowering exists, but it does not model or verify the complete ownership/write-back envelope. |
| 8 | **MISSING** | No checked direct/indirect/trait/closure/place matrix runs interpreter, optimized Wasm, full de-opt, single-lever de-opt, and an independent expected oracle. |
| 9 | **PARTIAL** | `__witchy_reowns` supplies one existing counter. Boundary repair, ownership-token repair, direct-storage access, indirect-envelope, and destination-forward counters are incomplete. |
| 10 | **MISSING** | The spec, callable reflection, diagnostics, and runnable book do not describe a shipped uniform access ABI. |

## RFC-0111 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **MISSING** | There is no canonical versioned `LayoutId`/descriptor driving all listed consumers; current shape/layout decisions are distributed across typeck, lowering, WIR, and runtime helpers. |
| 2 | **FAILING** | Type checking deliberately rejects `List(P)` with declared-packed `P` at parameter, return, and field boundaries. |
| 3 | **MISSING** | Generic callable specialization does not key or preserve packed physical layouts through construction, traversal, mutation, and return. |
| 4 | **MISSING** | Function values, closures, and traits do not carry an exact layout ID plus RFC-0110 access envelope. |
| 5 | **MISSING** | Host ABI metadata has no descriptor acceptance/marshal/reject protocol for specialized aggregates. Existing capability-reference classification remains a required safety baseline. |
| 6 | **MISSING** | Unique-result destination passing and zero-intermediate-allocation evidence do not exist. |
| 7 | **MISSING** | Whole-graph header-free selection and differential RC-backed equivalence do not exist. |
| 8 | **MISSING** | Fixed-layout closed sums have no complete descriptor/ABI/equality/drop/benchmark matrix. |
| 9 | **MISSING** | The benchmark corpus has no pinned scalar-only paired Rust leg or 1.25x/1.50x gates. |
| 10 | **MISSING** | No cross-lever acceptance slice jointly covers specialized layouts under checked heap, redzones, UAF checks, parity, runnable docs, and artifact compatibility. |
| 11 | **FAILING** | Current architecture and performance specs still state the universal-slot/confined-packed boundary rather than the proposed shipped matrix. |

## RFC-0112 acceptance criteria

| # | Status | Current evidence / exact gap |
| ---: | --- | --- |
| 1 | **MISSING** | Lifetimes parse in `let('a)`/`View(T, 'a)` positions, but nominal declarations cannot declare lifetime parameters or reflect/kind-check them. |
| 2 | **FAILING** | RFC-0083 intentionally rejects borrowed-view persistence in owned aggregates; fixed borrowed records/tuples cannot cross modules as owner-preserving values. |
| 3 | **FAILING** | `loans.rs` explicitly rejects persistence of a projection from an already-bound view and requires `.owned()`. |
| 4 | **PARTIAL** | Statement-identity last-use loans ship, including branches and loops, but facts lack a first-class projection/root/range representation. |
| 5 | **PARTIAL** | Function-value lifetime relations are checked today; nominal aggregate owner positions and the RFC-0110 unified access signature are absent. |
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
