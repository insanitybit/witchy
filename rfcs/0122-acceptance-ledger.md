# RFC-0122 acceptance ledger

This is the integration authority for RFC-0122. “Merged” requires master plus
the named evidence. “Queued” is not acceptance. Every other row remains open.

| Criterion | Track | State | Required evidence |
| --- | --- | --- | --- |
| 1 normal-mode reference exclusion | boundary | partial | normal parser/type/link diagnostic matrix |
| 2 conventional normal calls | boundary | open | proven/repair selection tests with no normal diagnostics |
| 3 normal interface filtering | boundary | open | imports, traits, aliases, Dynamic, reflection fixture matrix |
| 4 one opt source identity | contract/boundary | open | generated proven and repair entry identity tests |
| 5 proven versus repair parity | boundary/evidence | open | paired observable/write-back differential fixture |
| 6 owner-backed normal results | runtime/boundary | open | root-balance, escape, materialization counter tests |
| 7 opt syntax pipeline | contract | partial | merged `57954fe7`; add quote/highlight/full-stage parity |
| 8 uniform reference types | contract | partial | built-in/generic/nominal/container/trait/function-type matrix |
| 9 nominal lifetime versus reference distinction | contract/migration | open | parser, kind, reflection, migration tests |
| 10 migrated fixture parity | evidence | open | migration corpus matrix with both backends and counters |
| 11 shared loans | checker | partial | merged shared loan tests; add copy, escape, `var`, consume, drop matrix |
| 12 exclusive loans | checker/runtime/wasm | queued/open | queued `e07819bd`; affine moves, writes, suspension, no-copy proof remain |
| 13 mutable-to-shared conversion | checker/runtime | open | relinquish and shortened-reborrow tests |
| 14 owned qualifiers | checker | open | frozen/unique/local-unique combination diagnostics |
| 15 convention/reference orthogonality | contract/checker | partial | callable identity test; handle write-back/own behavior remains |
| 16 no opt-graph erasure | contract | partial | signature preservation; casts/traits/closures/adapters/tails matrix remains |
| 17 CFG precision | checker | partial | existing CFG facts; conditional returns, lending iterators, exact diagnostics remain |
| 18 aggregate affine roots | checker/runtime | partial | shared aggregate roots; affine move/copy/destructure/iteration remains |
| 19 interpreter/Wasm parity | runtime/wasm/evidence | open | forced-copy and direct-place differential fixture suite |
| 20 async and escape boundaries | checker/boundary | partial | shared escape cases; exclusive boundary matrix remains |
| 21 migration command | evidence/migration | open | rewrite report, ambiguity tests, legacy-free census |
| 22 performance telemetry | evidence | open | pinned corpus metric schema and before/after artifacts |

## Track contracts

| Interface | Owner track | Consumers |
| --- | --- | --- |
| `ReferenceKind` and lifetime-bearing callable identity | contract | checker, boundary, Wasm |
| `LoanFact` with owner root, projection, kind, origin, and affine state | checker | runtime, Wasm, evidence |
| callable effects and proven/repair adapter ABI | boundary | contract, runtime, Wasm |
| place-reference read/write semantic fixture | runtime | Wasm, evidence |

## Current integration sequence

1. Land the queued exclusive contract/checker foundation.
2. Rebase and land the explicit `&mut` call-contract slice without breaking the
   legacy shared-call migration surface.
3. Integrate runtime and forced-copy Wasm place semantics against the same
   differential fixtures.
4. Land boundary adapters only against the frozen callable-effect ABI.
5. Close each row with its listed evidence; no row is inferred from a green
   broad gate.
