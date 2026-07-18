# RFC-0081 current-truth and acceptance ledger

Audit date: 2026-07-18

Master baseline: `172f901cffe302ba1feff374e6d329fe0a19858d`

Recovery root: `integration/rfc0081-root-recovery`, based on the queued
RFC-0080 structural parent `impl/rfc0080-structural-block-builder` at
`ab372014e0b4a40ca22dd0e43c73e93551b71c99` so the RFC-0081 recovery does not
overwrite RFC-0080's `crates/witchy-interp/src/interpreter.rs` work.

Canonical recovery stack:

```text
integration/rfc0081-root-recovery
  -> integration/rfc0081-runtime-witnesses
  -> integration/rfc0081-receiver-conventions
  -> integration/rfc0081-supertrait-upcasts-recovery
  -> integration/rfc0081-receiver-safety
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
| 6 | **READY** | `integration/rfc0081-runtime-witnesses` adds interpreter execution to the compiled closed-witness dispatch root. Focused interpreter/lowerer tests plus `rfc0081_same_spelled_traits_dispatch_independently_on_both_backends` and the heterogeneous-list coverage prove both backends select the same witnesses; merge evidence remains required. |
| 7 | **DONE** | Master commit `1c6f7d22` emits one concrete GC payload box per closed witness using the payload's WIR kind plus a fixed `{structref, i32}` wrapper; structural tests and WIR validation guard reference-kind crossings. |
| 8 | **READY** | `integration/rfc0081-receiver-safety` proves bare, `let`, `var`, and `own` direct/interpreter/Wasm agreement in one differential program. Dynamic receiver and explicit-argument write-backs now reuse the ordinary typed nested-place reconstruction path. |
| 9 | **READY** | The receiver-safety differential covers tail-call, explicit-return, and `?` write-back, two non-overlapping projections of one root, alias rejection, `own` use-after-move rejection, and traps in both backends. Interpreter commits only after `run_callable`; Wasm commits only after `CallIndirectStoreMulti`, so a trap cannot expose a partial caller update. |
| 10 | **READY** | `integration/rfc0081-supertrait-upcasts-recovery` carries authenticated projection, interpreter and Wasm execution, qualified-identity diagnostics, a positive differential, and unrelated-upcast rejection. Focused type/interpreter/lower/root shards are green; merge evidence remains required. |
| 11 | **BROKEN** | Lowering refuses structural equality for `dyn`, but there is no complete adversarial rejection matrix for equality, ordering, hashing, reflection, serialization, type names, addresses, witnesses, or downcasts. |
| 12 | **BROKEN** | Existing construction reachability is conservative, but no checked-in footprint test proves every reachable witness adapter is included or that a reachable authority-using construction widens deterministically. |
| 13 | **BROKEN** | No normal-versus-`mode opt` existential differential matrix proves identical values and traps or documents the allocation/devirtualization non-promise. |
| 14 | **BROKEN** | Formatter frontend coverage exists, but `spec/language.md` and `book/src/backends.md` still say runtime dispatch is unavailable. There is no runnable heterogeneous-list or `var self` book example, migration guidance, or complete runtime diagnostic evidence. |
| 15 | **BROKEN** | No complete implementing stack has passed focused backend/adversarial shards and the serialized full gate. Prior root attempts were red, timed out, or failed fast-forward. |

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
| `integration/rfc0081-root-recovery` | **READY** | Canonical recovered dispatch root, queued explicitly after `impl/rfc0080-structural-block-builder`; obsolete duplicate queue entries are not parents. |
| `integration/rfc0081-runtime-witnesses` | **READY** | Canonical interpreter runtime and compiled-adapter slice, queued after the recovered root. |
| `integration/rfc0081-receiver-conventions` | **READY** | Canonical basic `var self`, `own self`, and explicit `var` argument adapter slice, queued after runtime witnesses. |
| `integration/rfc0081-supertrait-upcasts-recovery` | **READY** | Canonical authenticated-upcast slice, queued after receiver conventions. |
| `integration/rfc0081-receiver-safety` | **READY** | Canonical nested-place, alias/move, tail/explicit/`?`/trap parity slice, stacked after authenticated upcasts; the merge-queue journal remains authoritative for landing state. |

## Reconstructed semantic graph

```text
master foundations
  -> RFC-0080 queued structural stack (interpreter.rs owner)
  -> recovered RFC-0081 dispatch root
  -> interpreter owned-value/runtime dispatch + compiled adapters
  -> receiver and argument conventions
  -> authenticated supertrait projection/upcasts
  -> authority, tooling, differential tests, executable docs, and status closeout
```

Old queue change IDs and clean worktrees remain historical artifacts. They are
not implementation evidence and must not be used as parents once their semantic
commits have been recovered into this graph.
