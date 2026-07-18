# RFC-0081 current-truth and acceptance ledger

Audit date: 2026-07-18

Original audit baseline: `172f901cffe302ba1feff374e6d329fe0a19858d`

Current queue baseline: `7daad73600c5daa377cca332a49a75c05d6038d9`

Recovery root: `integration/rfc0081-root-current`, replayed patch-equivalently
onto the RFC-0080 owner's consolidated `integration/rfc0080-owned-syntax` at
`98929413067be1fa88d495886967a555592dc604`. This replaces the earlier lineage
through the red `impl/rfc0080-node-origins` queue record without rewriting
RFC-0080's `crates/witchy-interp/src/interpreter.rs` work.

Canonical recovery stack:

```text
integration/rfc0080-owned-syntax
  -> integration/rfc0081-root-current
  -> integration/rfc0081-runtime-current
  -> integration/rfc0081-receivers-current
  -> integration/rfc0081-upcasts-current
  -> integration/rfc0081-safety-current
  -> integration/rfc0081-public-current
  -> integration/rfc0081-tracking-current
```

Statuses:

- **DONE** — the criterion has checked-in executable or structural evidence on
  master.
- **READY** — a justified semantic change exists and has been recovered or can
  be recovered onto the current integration graph, but is not merged evidence.
- **BROKEN** — implementation or durable evidence is absent, stale, failing, or
  contradicted by the current public contract.
- **OBSOLETE** — an old branch or queue entry is patch-equivalent to landed or
  recovered work, duplicates another tip, has a false dependency, or has been
  superseded by the reconstructed graph.

## Acceptance criteria

| # | Status | Current evidence or exact gap |
| ---: | --- | --- |
| 1 | **DONE** | Parser/formatter/type-resolution tests cover `dyn Trait(args...)` and qualified heads in type positions, alias/import identity, distinct same-spelled declarations, and ambiguous bare imports. |
| 2 | **DONE** | Master commit `58850364` records directed constructions after final monomorphization at annotations, arguments, returns, assignments, aggregate and constructor slots, and explicit casts; checker tests retain concrete inference and `var dyn` invariance. |
| 3 | **DONE** | Type-checker tests cover receiver-less methods, unresolved trait arguments, method-local generics, bare and nested forbidden `Self`, receiver-borrowed results, `PartialEq`, and diagnostics listing every blocker. |
| 4 | **DONE** | Type-checker and linked-pipeline tests reject direct and transitive capability payloads through records, sums, tuples, `Option`, and collections before lowering. |
| 5 | **DONE** | The landed witness substrate (`dbfa1579`, `5f89632c`, `220cad06`) pins deterministic witness IDs, concrete-independent slot linearization, conditional impl selection, and transitive supertrait slot deduplication. |
| 6 | **READY** | `integration/rfc0081-runtime-current` adds interpreter execution to the compiled closed-witness dispatch root. Focused interpreter/lowerer tests plus `rfc0081_same_spelled_traits_dispatch_independently_on_both_backends` and the heterogeneous-list coverage prove both backends select the same witnesses; merge evidence remains required. |
| 7 | **DONE** | Master commit `1c6f7d22` emits one concrete GC payload box per closed witness using the payload's WIR kind plus a fixed `{structref, i32}` wrapper; structural tests and WIR validation guard reference-kind crossings. |
| 8 | **READY** | `integration/rfc0081-safety-current` proves bare, `let`, `var`, and `own` direct/interpreter/Wasm agreement in one differential program. Dynamic receiver and explicit-argument write-backs now reuse the ordinary typed nested-place reconstruction path. |
| 9 | **READY** | The receiver-safety differential covers tail-call, explicit-return, and `?` write-back, two non-overlapping projections of one root, alias rejection, `own` use-after-move rejection, and traps in both backends. Interpreter commits only after `run_callable`; Wasm commits only after `CallIndirectStoreMulti`, so a trap cannot expose a partial caller update. |
| 10 | **READY** | `integration/rfc0081-upcasts-current` carries authenticated projection, interpreter and Wasm execution, qualified-identity diagnostics, a positive differential, and unrelated-upcast rejection. Focused type/interpreter/lower/root shards are green; merge evidence remains required. |
| 11 | **READY** | `integration/rfc0081-public-current` adds a checked rejection matrix for equality, ordering, hashing, reflection, serialization, type names, addresses, witness identity, and downcasts. Equality now rejects `dyn` at every container depth and explicitly forbids payload-address/witness fallbacks. |
| 12 | **READY** | A WIR footprint test proves one adapter per reachable closed construction, excludes an unreachable authority-using witness, includes every reachable adapter, widens to the `Console.print` host import when that witness becomes reachable, and reproduces byte-identical WAT. |
| 13 | **READY** | A four-way normal/`mode opt` interpreter/Wasm differential proves identical heterogeneous dispatch values and divide-by-zero traps. Spec and book state that opt mode promises neither allocation removal nor devirtualization. |
| 14 | **READY** | The public `witchy check` stage gate is removed while safety validation remains. Formatter coverage, `expand`, LSP display/diagnostics, operation/missing-witness diagnostics, spec language, runnable heterogeneous-list and `var self` book programs, and migration guidance are checked in on the public-contract branch. |
| 15 | **READY** | The complete implementing stack has focused type/interpreter/lowerer/differential/adversarial evidence, and `./scripts/check.sh --wasm` is green with all 127 runnable book blocks agreeing. Browser coverage and the serialized full gate remain required merge evidence before this row or the RFC status may become **DONE** / `implemented`. |

## Preserved branch and queue disposition

| Branch or duplicate set | Status | Disposition |
| --- | --- | --- |
| `fix/rfc0081-trait-identity` | **OBSOLETE** | Both commits are patch-equivalent to changes on master. |
| `impl/rfc0081-witness-substrate`, `impl/rfc0081-interpreter-witnesses` | **OBSOLETE** | Duplicate tip `9f598562`; all three commits are patch-equivalent to the landed witness substrate. The interpreter label never contained interpreter execution. |
| `impl/rfc0081-directed-coercions` | **DONE** | Tip `58850364` is an ancestor of master. |
| `fix/rfc0081-interpreter-exhaustive`, `fix/rfc0081-interpreter-exhaustive-clippy`, `impl/rfc0081-wasm-witness-dispatch` | **OBSOLETE** | Three queue entries share tip `edc8c27`; adapters incorrectly depend on two aliases of the same semantic change. Recovered once as `d269b746`, `ff3b08f8`, `d215f706` on the RFC-0080 parent. |
| `impl/rfc0081-wasm-witness-stack-fresh` | **OBSOLETE** | A second rebased lineage of the same dispatch/adapters/traversal/Clippy patches. Its prior gates timed out; its unique semantics are recovered once on the integration branch. |
| `impl/rfc0081-wasm-witness-adapters` | **OBSOLETE** | Its six semantic commits are recovered once as the root integration stack through `59348769`; the old queue dependency list contains duplicate aliases. |
| `impl/rfc0081-wasm-witness-receivers` | **READY** | One recoverable commit for compiled `var self`; requires stronger success/trap evidence and interpreter parity. |
| `impl/rfc0081-wasm-witness-own`, `impl/rfc0081-wasm-witness-supertraits` | **READY** / **OBSOLETE** | The branches share tip `a85f42ca`. The `own` commit is recoverable; the `supertraits` label contains no supertrait work and is obsolete. |
| `impl/rfc0081-wasm-witness-var-args`, `impl/rfc0081-wasm-witness-var-places` | **READY** / **OBSOLETE** | The branches share tip `e8a77f16`. The dynamic `var` argument commit is recoverable; the second queue/worktree label is duplicate. Nested-place and alias evidence is still missing. |
| `impl/rfc0081-interpreter-runtime` | **READY** | The final commit adds existential values and dispatch in the interpreter, but its parent is the obsolete duplicate root and it must be replayed after RFC-0080 without overwriting that stack. |
| `impl/rfc0081-supertrait-upcasts` | **READY** | Contains authenticated supertrait projection planning only; it is not a complete upcast slice. |
| `impl/rfc0081-upcast-integration` | **READY** | Contains the recoverable complete upcast sequence, but also replays the obsolete fresh root and uses stale queue parents. Only its unique commits should survive. |
| `docs/language-rfc-link`, `docs/language-rfc0081-link` | **OBSOLETE** | Different tips carry the same patch ID. The one-line link is insufficient for criterion 14 and will be subsumed by public-contract closeout. |
| `integration/rfc0081-root-recovery` through `integration/rfc0081-public-contract` | **OBSOLETE** | Patch-equivalent first recovery lineage. Its root depended on the old RFC-0080 block-builder change ID, which inherited the unrelated red node-origins attempt; the source tips remain preserved but are no longer canonical queue parents. |
| `integration/rfc0081-root-current` | **READY** | Canonical recovered dispatch root, nine commits replayed patch-equivalently onto `integration/rfc0080-owned-syntax`; the old duplicate RFC-0081 entries are not parents. |
| `integration/rfc0081-runtime-current` | **READY** | Canonical interpreter runtime and compiled-adapter slice, queued only after the current root. |
| `integration/rfc0081-receivers-current` | **READY** | Canonical basic `var self`, `own self`, and explicit `var` argument adapter slice, queued only after current runtime witnesses. |
| `integration/rfc0081-upcasts-current` | **READY** | Canonical authenticated-upcast slice, queued only after current receiver conventions. |
| `integration/rfc0081-safety-current` | **READY** | Canonical nested-place, alias/move, tail/explicit/`?`/trap parity slice; the merge-queue journal remains authoritative for landing state. |
| `integration/rfc0081-public-current` | **READY** | Canonical operation-absence, footprint, normal/opt, tooling, spec/book, migration, and public-checker closure; focused type/RFC/Clippy/docs/Wasm shards are green. |
| `integration/rfc0081-tracking-current` | **READY** | Docs-only truth reconciliation after the dependency repair. It does not claim implementation until the complete current stack is merged and a fresh-master gate proves it. |

## Reconstructed semantic graph

```text
master foundations
  -> RFC-0080 consolidated owned-syntax stack (interpreter.rs owner)
  -> recovered RFC-0081 dispatch root
  -> interpreter owned-value/runtime dispatch + compiled adapters
  -> receiver and argument conventions
  -> authenticated supertrait projection/upcasts
  -> authority, tooling, differential tests, executable docs, and status closeout
```

Old queue change IDs and clean worktrees remain historical artifacts. They are
not implementation evidence and are not parents of the `*-current` graph. A
`READY` current entry still requires a terminal merged journal event before it
can become `DONE`.
