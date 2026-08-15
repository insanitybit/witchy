# RFC-0122 acceptance ledger

This is the integration authority for RFC-0122. “PROVEN” requires current
`master` plus the named evidence. A green queue item is not acceptance. Every
other row remains open.

Audit target: `9c8081c80b208ff9e83562c5677fd18b22d3998a` on `master`
(2026-08-15). The linked RFC is accepted; this ledger determines when it is
implemented.

| Criterion | Track | State | Required evidence |
| --- | --- | --- | --- |
| 1 normal-mode reference exclusion | boundary | partial | syntax is mode-gated; add parser/type/link diagnostic matrix for every reference-bearing position |
| 2 conventional normal calls | boundary | partial | `rfc0122_normal_callers_use_conventional_opt_access_on_both_backends` proves reference-free `let`/`var`/`own` calls; proven/repair selection remains unimplemented |
| 3 normal interface filtering | boundary | open | imports, traits, aliases, Dynamic, reflection fixture matrix |
| 4 one opt source identity | contract/boundary | open | generated proven and repair entry identity tests |
| 5 proven versus repair parity | boundary/evidence | open | paired observable/write-back differential fixture |
| 6 owner-backed normal results | runtime/boundary | open | root-balance, escape, materialization counter tests |
| 7 opt syntax pipeline | contract | partial | merged `57954fe7`; add quote/highlight/full-stage parity |
| 8 uniform reference types | contract | partial | built-in/generic/nominal/container/trait/function-type matrix |
| 9 nominal lifetime versus reference distinction | contract/migration | open | parser, kind, reflection, migration tests |
| 10 migrated fixture parity | evidence | open | migration corpus matrix with both backends and counters |
| 11 shared loans | checker | partial | merged shared loan tests; add copy, escape, `var`, consume, drop matrix |
| 12 exclusive loans | checker/runtime/wasm | partial | `rfc0122_local_exclusive_reference_write_agrees_on_both_backends`, returned-reborrow, function-value, and closure projected-place fixtures prove executable writes; affine moves, parent suspension, and no-copy proof remain |
| 13 mutable-to-shared conversion | checker/runtime | partial | merged `bcea90b4` relinquishes the old `&mut` handle; checker covers short shared reborrow; runtime parity and shortening matrix remain |
| 14 owned qualifiers | checker | open | frozen/unique/local-unique combination diagnostics |
| 15 convention/reference orthogonality | contract/checker | partial | callable identity test; handle write-back/own behavior remains |
| 16 no opt-graph erasure | contract | partial | signature preservation; casts/traits/closures/adapters/tails matrix remains |
| 17 CFG precision | checker | partial | existing CFG facts; conditional returns, lending iterators, exact diagnostics remain |
| 18 aggregate affine roots | checker/runtime | partial | shared aggregate roots; affine move/copy/destructure/iteration remains |
| 19 interpreter/Wasm parity | runtime/wasm/evidence | partial | direct scalar and aggregate projected reads/writes agree on both backends through direct calls, function values, and closures (`rfc0122_*` ownership fixtures); forced-copy/direct-place, traps, cleanup, and full type matrix remain |
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

1. Generalize the executable carrier from `Int` and projected Bool slots to all
   referenceable scalar and aggregate representations, then add structured-exit
   and reborrow fixtures.
2. Complete affine checker state, parent suspension, and no-copy eligibility
   from the frozen `ReferenceKind` / `LoanFact` contract.
3. Integrate runtime and forced-copy/direct-place Wasm semantics against the
   same differential fixtures.
4. Land boundary adapters only against the frozen callable-effect ABI.
5. Close each row with its listed evidence; no row is inferred from a green
   broad gate.
