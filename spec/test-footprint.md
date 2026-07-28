# Test footprint and evidence layers

`./scripts/test-footprint.sh` is the authoritative measurement for dedicated
Rust test code. It reports disjoint integration, example-matrix,
extracted-crate-test, and support sets; `explicit_total` is the release metric.
Use `--files` when reviewing ownership or a proposed deletion.

## Authorities

- Crate unit tests localize parser, formatter, type-checker, WIR, interpreter,
  runtime, and lowering contracts.
- `src/example_tests/` supplies independently expected semantics and compares
  the interpreter with compiled Wasm. Parity is evidence of agreement, not an
  independent specification oracle.
- `tests/misc/semantic_conformance.rs` states exact values, rejection
  diagnostics, and capability footprints independently. Its seeded shared-stage
  mutation proves that parity can agree on a wrong implementation while the
  independent expectation rejects it.
- `tests/e2e.rs`, `tests/browser.rs`, and the runnable book retain real-process,
  authenticated package, confinement, browser-host, and documentation evidence.
- Mutation and fault-injection tests remain required for retained semantic and
  ABI authorities.

Security, provenance, authentication, capability denial, diagnostics, source
locations, ABI shapes, historical regressions, Unicode and malformed-input
boundaries, and real TCP behavior require an explicit retained authority. A
fixture helper is worthwhile only when it removes more setup than it adds and
does not hide the contract under test.

## Routing and current measurement

`./scripts/test-for-paths.sh <paths>` selects focused checks from changed
paths; its `--run` form executes them. Full workspace, Clippy, Wasm, browser,
and book validation is serialized by `./scripts/merge-queue.sh submit`.

At master commit `61f298c2` on 2026-07-28, the measured footprint is:

| layer | files | Rust lines |
| --- | ---: | ---: |
| integration | 96 | 19,642 |
| example matrix | 56 | 18,271 |
| extracted crate tests | 14 | 14,245 |
| explicit total | 166 | 52,158 |
| support | 20 | 8,383 |
| explicit plus support | 186 | 60,541 |

The normalized baselines are 56,984 explicit lines and 65,435 total lines.
Recent serialized gates were green for the merged browser-driver, sanitizer,
string-boundary, and scalar-codegen slices. The queue must remain the source of
truth for exact gate timing; recent recorded gate durations ranged from 191 s
to 2,539 s, with CPU contention explaining the outliers.

The normalized reduction is currently 4,826 explicit lines and 4,894 lines
including support; the remaining distance to 40,000 explicit lines is 12,158.
The footprint reduction remains in progress. This document records the
retained evidence model and current measurement; it does not waive the goal's
15,000-line deletion, API-shrink, or final-gate requirements.

## Current retained-census record

The completed deletion audit retained the following authorities after review:

- capability, confinement, provenance, authentication, and browser-host tests;
- ABI, WIR, ownership, region, allocation, and runtime-shape tests;
- malformed-input, Unicode, URL, typed-error, source-location, and historical
  regression diagnostics;
- interpreter/Wasm parity with explicit expected values, plus the independent
  `semantic_conformance` and mutation/fault-injection oracles;
- JSON, concurrency, traits, RFC-0080, RFC-0081, package-manager, testkit,
  test-host, merge-queue, runnable-book, and end-to-end security layers.

Merged reduction slices on this measurement include standard-library smokes,
quote parity, closure parity, sandbox parity, region/capability smokes,
mutation trampolines, network duplicates, JSON API fixture consolidation,
the RFC-0080 compiled-output helper, crypto fixture pruning, and traits
protocol-fixture consolidation. The browser gate now probes for actual
WebAssembly JSPI support: capable Node hosts run the capability-host driver,
while hosts that expose no JSPI constructors report an explicit skip; pure
Node browser drivers remain covered on those hosts.
