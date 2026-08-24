# RFC-0155 acceptance ledger

This ledger is the durable completion index for RFC-0155. The RFC remains
proposed and this ledger remains incomplete until every core row has landed
evidence. Raw timing, profile, fuzz, and adversarial artifacts remain outside
source control, but each recorded artifact must identify its source commit,
host, toolchain, command, configuration, and digest.

## Completion contract

- A cache row is complete only when its production read and write paths use the
  authenticated policy or persistent caching is deliberately disabled.
- An adversarial row is complete only after the named attack is executed against
  the production path. Code inspection and unit tests of a helper are not
  substitutes.
- A queue row is complete only after its terminal `merged` journal event and
  landed commit are recorded here.
- Performance comparisons use the same host, isolated target directory, source
  state, commands, concurrency, and warmed build graph. Report at least three
  raw samples and the median.
- RFC status changes only after a requirement-by-requirement audit finds no
  `pending`, `partial`, `failed`, or `unverified` core row.

## Dependency graph

```text
canonical record + authenticator + cache-off policy
    |-- source and embedded Wasm caches
    |-- optimized-Wasm cache -- pinned optimizer identity
    |-- stdlib AST cache
    |-- build-result cache
    `-- native artifact cache -- authenticate-before-deserialize contract

trusted executable cache independence ---------------- independent critical path

all production migrations + adversarial/fuzz evidence + performance evidence
    `-- completion audit and RFC status
```

The canonical record interface is frozen before cache migrations. Trusted
executable cache independence can land independently. Native artifact caching
may remain disabled if its unsafe deserialization contract cannot be proven.

## Executable-state inventory

| Producer / consumer | Current location | Required terminal policy | State | Evidence |
|---|---|---|---|---|
| Trusted executable embedded Wasm | `src/commands/wasm_exec.rs`, `crates/witchy-runtime/src/runtime.rs` | Never read or write mutable executable cache state | partial | `security/trusted-exe-cache-independent`; focused unit test passed, e2e and queue evidence pending |
| Source-to-Wasm cache | `src/commands/compile.rs` (`witchy/src`) | Authenticated exact input-to-Wasm record or cache off | pending | none |
| Embedded-module Wasm cache | `src/commands/compile.rs` (`witchy/embedded`) | Authenticated exact embedded input-to-Wasm record or cache off | pending | none |
| Optimized-Wasm cache | `crates/witchy-runtime/src/runtime.rs` (`witchy/optimized-wasm`) | Authenticated record with pinned optimizer recipe or cache off | pending | none |
| Wasmtime native artifact cache | `crates/witchy-runtime/src/runtime.rs` (`witchy/wasm`) | Authenticate and bound before deserialization with documented safety contract, or cache off | pending | none |
| Bundled stdlib AST cache | `crates/witchy-syntax/src/linker.rs` (`witchy/stdlib` and test cache) | Authenticated exact source-to-AST record or cache off | pending | none |
| Grimoire build-result cache | `projects/grimoire/src/gm_engine.witchy` (`.witchy-build-cache`) | Authenticated build-input-to-output record or cache off | pending | none |

Package downloads and non-executable browser/content caches are not executable
cache records. If a later consumer compiles or loads their bytes as code, that
consumer must freeze and authenticate the executable relationship covered by
this ledger.

## Core acceptance criteria

| RFC criterion | State | Required evidence | Landed evidence |
|---|---|---|---|
| 1. Trusted executable leaves an absent cache root absent | partial | Production launch e2e with absent `XDG_CACHE_HOME`, plus optimized-Wasm and native-cache policy unit test | focused unit test passed on `security/trusted-exe-cache-independent`; remaining evidence pending |
| 2. Expected-input malicious optimized-Wasm envelope without valid MAC is rejected | pending | Production cache attack fixture | none |
| 3. Self-consistent malicious record and artifact are rejected | pending | Production cache attack fixture for every enabled artifact kind | none |
| 4. Cross-input, target, configuration, and epoch replays fail; identical-input sharing is safe | pending | Canonical-record tests and at least one production replay fixture | none |
| 5. Invalid or unavailable credentials run without persistent access or prompts | pending | Injected-provider tests and headless subprocess test | none |
| 6. Bounds and authentication precede artifact consumption; native safety contract documented | pending | Instrumented ordering test, malformed artifact fixture, and native-cache decision | none |
| 7. Footprints, imports, bindings, and grants are independent of cache state | pending | Cache-metadata mutation tests and current-input derivation audit | none |
| 8. No production unauthenticated override exists | pending | CLI, environment, config, and source census | none |
| 9. External optimizer identity is authenticated and ambient `PATH` cannot sign output | pending | Substituted-`PATH` production fixture and exact-executable recipe evidence | none |
| 10. Hashed producer input is the representation compiled, with no mutable reopen | pending | File-swap race fixture and source-path audit | none |
| 11. Startup and cache-hit performance is quantified | pending | Performance matrix below | none |
| 12. Canonical decoder, bounds, dispatch, and lookup races are fuzzed | pending | Reproducible fuzz commands, duration, corpus identity, and zero unresolved findings | none |

## Optional claim gates

These rows do not delay the authenticated-cache core. Witchy must not claim an
optional property unless its row is complete.

| Claim | State | Required evidence |
|---|---|---|
| Guest cache exclusion | not claimed | Parent/direct grants, symlink, hard link, rename, truncation, deletion, open handle, and concurrent replacement fixtures |
| Same-user native-child containment | not claimed | Child cannot access either cache state or authenticator operation |
| Linux Landlock descendant containment | not claimed | Every granted root tested for worker and descendant reachability |
| Unsupported-platform confinement reporting | not claimed | Negative platform fixture reports unavailable rather than enforced |

## Performance acceptance matrix

All measurements are paired against a pre-change master baseline. Security
changes must not make Witchy's own test suite depend on repeated cold Binaryen
or Cranelift compilation.

| Workload | Modes | Samples | Acceptance | State / artifact |
|---|---|---:|---|---|
| `cargo nextest run --workspace` test execution | baseline and final, warmed isolated target | at least 3 each | final median no more than 5 percent slower; investigate any greater shift with per-test timing | pending |
| `cargo test --workspace --no-run` build graph | baseline and final, warmed isolated target | at least 3 each | no material rebuild amplification; final median no more than 5 percent slower | pending |
| Small, medium, and large source launch | authenticated cold, authenticated hit, unavailable | at least 5 each | report median and raw samples; authenticated hits retain the dominant compile savings | pending |
| Trusted executable launch | pre-change and cache-independent | at least 5 each | report startup cost and separate native compilation from guest execution | pending |
| Authenticator access | available warm, first access, unavailable/headless | at least 10 each | no prompt; process-local reuse measured separately | pending |
| Corrupted cache recovery | each enabled record kind | at least 3 each | safe miss; no unbounded retry or repeated same-process credential lookup | pending |

The 5 percent suite threshold is a promotion gate, not a claim that every
individual sample is stable within 5 percent. If environmental noise exceeds
that tolerance, increase repetitions and report dispersion rather than
discarding inconvenient samples.

## Implementation tracks

| Track | State | Shared contract / deliverable | Branch | Focused evidence | Terminal queue evidence |
|---|---|---|---|---|---|
| 0 ledger and baseline | in progress | This ledger; baseline build/test/startup artifacts | `impl/rfc0155-authenticated-caches` | pending | pending |
| 1 trusted distribution | partial | Cache-independent trusted-executable runtime | `security/trusted-exe-cache-independent` | unit passed; e2e running | pending |
| 2 cache protocol | planned | Canonical bounded record, authenticator abstraction, fail-closed policy | pending | pending | pending |
| 3 portable cache migrations | planned | Source, embedded, optimized Wasm, and stdlib AST | pending | pending | pending |
| 4 build and native decisions | planned | Build-result policy and authenticated native cache or explicit disable | pending | pending | pending |
| 5 tool and race closure | planned | Exact optimizer identity, frozen inputs, same-open bytes | pending | pending | pending |
| 6 adversarial and fuzz evidence | planned | Production attack matrix and fuzz artifacts | pending | pending | pending |
| 7 performance and closeout | planned | Matched performance matrix, full audit, RFC status | pending | pending | pending |

## Baseline identity

Baseline measurement is pending the active merge gate. Record the exact landed
master commit here before running any measurement; do not reuse stale metrics
from `scratch/dev-metrics`.
