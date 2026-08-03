# Glamour performance evidence

## Baseline command

Build Witchy normally, then run:

```sh
node web/witchy-runtime/glamour-baseline.mjs target/debug/witchy
```

The command compiles the canonical counter in a fresh temporary directory,
mounts it through the real JSON host, dispatches 100 increments, verifies the
final model, and prints one `witchy.glamour.baseline.v1` JSON document.

The report contains:

- host OS, architecture, and Node version;
- compiled Wasm byte size;
- mount and dispatch timings;
- JSON bridge step and UTF-8 byte totals;
- Wasm linear-memory pages and observed JavaScript heap delta;
- exact fake-DOM operation counts.

Timing and JavaScript heap deltas are observations, not deterministic test
goldens. Schema, final state, transport totals, linear-memory bounds, and DOM
operation relationships are testable invariants. Release reports retain raw
JSON and identify the exact commit and host.

## Benchmark hosts

Local runs are exploratory and may use any maintained Node release. Release
claims require:

- a pinned clean macOS arm64 host;
- a pinned clean Linux x86-64 host;
- fixed performance/power mode and no concurrent build or test load;
- warmup separated from measured samples;
- at least 30 samples, raw results retained, median and p95 reported;
- the exact Witchy commit, compiler mode, browser/Node version, and command.

Browser release evidence runs the same application corpus on current stable
Chromium, Firefox, and WebKit, plus their immediately previous stable release
where the automation provider supports it. Mobile evidence uses current Safari
on iOS and Chrome on Android before a 1.0 performance claim.

No absolute comparison to React, Vue, Svelte, Solid, Lit, Leptos, or another
framework is valid unless the compared implementation, workload, build mode,
browser, hardware, sampling method, and raw evidence are published together.

## Phase 3 keyed workload

Run the normalized JSON-reference and optimized-binary paths against the same
compiled source:

```sh
node web/witchy-runtime/glamour-phase3-performance.mjs target/debug/witchy
```

The command performs three warmups and 30 measured samples of mount plus 100
keyed counter interactions. It emits one
`witchy.glamour.phase3-performance.v1` record containing the exact commit,
dirty-worktree flag, host, artifact size, median and p95 timings, transport
bytes, operation counts, and Wasm pages for both paths.

The checked invariants are independent of host timing:

- the artifact is at most 2 MiB;
- linear memory remains within eight Wasm pages;
- the optimized root has one delegated listener;
- optimized mount emits five operations;
- each interaction emits one text patch and one minimal keyed move.

Local timing fields are exploratory. A release record is controlled evidence
only when `dirty` is false and the host meets the pinned-host requirements
above. CI retains raw records rather than copying one developer-machine timing
into a golden file.

## Dashboard schema

The public dashboard consumes append-only records:

```json
{
  "schema": "witchy.glamour.dashboard.v1",
  "commit": "full git commit",
  "recordedAt": "RFC 3339 timestamp",
  "hostId": "documented controlled host",
  "browser": "engine and exact version",
  "scenario": "counter-increment",
  "samples": 30,
  "artifactBytes": 0,
  "mountMs": {"median": 0, "p95": 0},
  "interactionMs": {"median": 0, "p95": 0},
  "transportBytes": {"median": 0, "p95": 0},
  "domOperations": {"median": 0, "p95": 0},
  "wasmMemoryPages": {"median": 0, "p95": 0},
  "exceptions": []
}
```

An exception names an owner, rationale, issue, approval date, and expiry. The
dashboard does not turn missing measurements into zeroes.
