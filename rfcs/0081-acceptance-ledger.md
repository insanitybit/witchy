# RFC-0081 current-truth and acceptance ledger

Audit date: 2026-07-18; implementation closeout: 2026-07-18

Original audit baseline: `172f901cffe302ba1feff374e6d329fe0a19858d`

Implemented master: `111b4236e071b9563c48d02bdfce6776fc8fb705`

The abandoned branches were audited and recovered into a dependency-ordered
stack without rewriting the RFC-0080 owner's
`crates/witchy-interp/src/interpreter.rs` work. The consolidated integration
branch `integration/rfc0081-foundation-current` then landed that semantic stack
at `111b4236` through the coordinator's serialized full gate. Its gate log is
`state/merge-queue/logs/20260718-222122-integration~rfc0081-foundation-current-203-19.log`.

Reconstructed recovery stack:

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
| 6 | **DONE** | Master contains interpreter owned existential construction/dispatch and compiled closed-witness adapters. `rfc0081_same_spelled_traits_dispatch_independently_on_both_backends`, heterogeneous-list coverage, and the full gate prove both backends select the same authenticated witness. |
| 7 | **DONE** | Master commit `1c6f7d22` emits one concrete GC payload box per closed witness using the payload's WIR kind plus a fixed `{structref, i32}` wrapper; structural tests and WIR validation guard reference-kind crossings. |
| 8 | **DONE** | Master commit `d2e513b6` proves bare, `let`, `var`, and `own` direct/interpreter/Wasm agreement in one differential program. Dynamic receiver and explicit-argument write-backs reuse the ordinary typed nested-place reconstruction path. |
| 9 | **DONE** | The landed receiver-safety differential covers tail-call, explicit-return, and `?` write-back, two non-overlapping projections of one root, alias rejection, `own` use-after-move rejection, and traps in both backends. Interpreter and Wasm write back only after successful calls, so a trap cannot expose a partial caller update. |
| 10 | **DONE** | Master contains authenticated supertrait witness projection, interpreter and Wasm execution, qualified-identity diagnostics, positive differential coverage, and unrelated/forged-upcast rejection before execution. |
| 11 | **DONE** | Master commit `ea173c94` adds a checked rejection matrix for equality, ordering, hashing, reflection, serialization, type names, addresses, witness identity, and downcasts. Equality rejects `dyn` at every container depth and has no payload-address or witness fallback. |
| 12 | **DONE** | The landed WIR footprint test proves one adapter per reachable closed construction, excludes an unreachable authority-using witness, includes every reachable adapter, widens to the `Console.print` host import when that witness becomes reachable, and reproduces byte-identical WAT. |
| 13 | **DONE** | A landed four-way normal/`mode opt` interpreter/Wasm differential proves identical heterogeneous dispatch values and divide-by-zero traps. Spec and book state that opt mode promises neither allocation removal nor devirtualization. |
| 14 | **DONE** | Formatter coverage, `expand`, LSP display/diagnostics, operation/missing-witness diagnostics, language spec, runnable heterogeneous-list and `var self` book programs, and migration guidance are merged. The public `witchy check` stage gate is removed while safety validation remains. |
| 15 | **DONE** | The coordinator landed `111b4236` after 2,311/2,311 workspace tests, formatting, deny-warnings Clippy, the Wasm playground build, and all 127 runnable browser-book blocks passed. Earlier focused type/interpreter/lowerer/differential/adversarial and eight-browser-test shards also passed on the reconstructed stack. |

## Preserved branch and queue disposition

| Branch or duplicate set | Status | Disposition |
| --- | --- | --- |
| `fix/rfc0081-trait-identity` | **OBSOLETE** | Both commits are patch-equivalent to changes on master. |
| `impl/rfc0081-witness-substrate`, `impl/rfc0081-interpreter-witnesses` | **OBSOLETE** | Duplicate tip `9f598562`; all three commits are patch-equivalent to the landed witness substrate. The interpreter label never contained interpreter execution. |
| `impl/rfc0081-directed-coercions` | **DONE** | Tip `58850364` is an ancestor of master. |
| `fix/rfc0081-interpreter-exhaustive`, `fix/rfc0081-interpreter-exhaustive-clippy`, `impl/rfc0081-wasm-witness-dispatch` | **OBSOLETE** | Three queue entries share tip `edc8c27`; adapters incorrectly depend on two aliases of the same semantic change. Recovered once as `d269b746`, `ff3b08f8`, `d215f706` on the RFC-0080 parent. |
| `impl/rfc0081-wasm-witness-stack-fresh` | **OBSOLETE** | A second rebased lineage of the same dispatch/adapters/traversal/Clippy patches. Its prior gates timed out; its unique semantics are recovered once on the integration branch. |
| `impl/rfc0081-wasm-witness-adapters` | **OBSOLETE** | Its six semantic commits are recovered once as the root integration stack through `59348769`; the old queue dependency list contains duplicate aliases. |
| `fix/rfc0081-foundation-gate` | **OBSOLETE** | All 40 submitted patches are represented on master; the queue record is a stale pre-consolidation repair tip. |
| `impl/rfc0081-wasm-witness-receivers` | **OBSOLETE** | Its compiled `var self` semantics were recovered with stronger success/trap and interpreter-parity evidence and are now on master. |
| `impl/rfc0081-wasm-witness-own`, `impl/rfc0081-wasm-witness-supertraits` | **OBSOLETE** | The branches share tip `a85f42ca`; the `own` semantics are merged and the `supertraits` label never contained supertrait work. |
| `impl/rfc0081-wasm-witness-var-args`, `impl/rfc0081-wasm-witness-var-places` | **OBSOLETE** | The branches share tip `e8a77f16`; the dynamic `var` argument semantics plus nested-place and alias evidence are merged. |
| `impl/rfc0081-interpreter-runtime` | **OBSOLETE** | Its unique existential value and interpreter dispatch semantics were replayed after RFC-0080 and landed without adopting the obsolete parent. |
| `impl/rfc0081-supertrait-upcasts` | **OBSOLETE** | Its projection planning was completed by the authenticated upcast slice now on master. |
| `impl/rfc0081-upcast-integration` | **OBSOLETE** | Its unique upcast commits were recovered; its replayed root and stale queue parents were discarded. |
| `docs/language-rfc-link`, `docs/language-rfc0081-link` | **OBSOLETE** | Different tips carry the same patch ID. The one-line link is insufficient for criterion 14 and is subsumed by the landed public contract. |
| `integration/rfc0081-root-recovery` through `integration/rfc0081-public-contract` | **OBSOLETE** | Patch-equivalent first recovery lineage. Its root depended on the old RFC-0080 block-builder change ID, which inherited the unrelated red node-origins attempt; the source tips remain preserved but are no longer canonical queue parents. |
| `integration/rfc0081-root-current` through `integration/rfc0081-tracking-current` | **OBSOLETE** | This justified canonical chain was overtaken by the consolidated `integration/rfc0081-foundation-current` landing. Its refs remain preserved, but its queued records are not independent merge evidence and must not re-gate duplicate semantics. |
| `integration/rfc0081-foundation-current` | **DONE** | Submitted tip `3ce42010` landed as master `111b4236` in the serialized green gate. It contains the recovered root, runtime, receivers, upcasts, safety, public contract, tracking, and integration-gate repairs. |

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

Old queue change IDs, branch tips, and clean worktrees remain preserved
historical artifacts. They are not implementation evidence. The terminal
`merged` journal event for `integration/rfc0081-foundation-current`, the landed
master tree, and its green serialized gate are the implementation evidence.
