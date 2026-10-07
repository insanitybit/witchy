# Typed receive-frame benchmark

The first state-machine slice lowers eligible `for await` receivers into a persistent scheduler frame. Producers keep their existing lowering. Mutable Int and Bool carries live in the frame; bodies with suspension, control escapes, or non-scalar captures retain ordinary lowering. Each invocation gets fresh state. Scheduling and channel closure preserve the existing order within each VM; there is no ordering contract between VMs.

## Reproduction

Build both revisions with the same Rust toolchain and release profile, save each binary, and snapshot each revision’s `std` directory at build time. Release binaries embed the standard library and ignore `WITCHY_STD_DIR`; rebuild after every std edit. The snapshot arguments record build provenance and do not select std at runtime. Run from the candidate checkout:

```sh
python3 scripts/async-frame-bench.py \
  --baseline /absolute/path/to/baseline-binary \
  --candidate /absolute/path/to/candidate-binary \
  --baseline-std /absolute/path/to/baseline/std \
  --candidate-std /absolute/path/to/candidate/std \
  --output /absolute/path/to/new-results-directory \
  --runs 12
```

Repeat `--workload <name>` to select a subset for a causal experiment; the default runs all five rows. The runner uses macOS `/usr/bin/time -lp`. It saves binary, standard-library, fixture, and harness hashes, validates every checksum, runs two warmups, and alternates twelve measured baseline/candidate pairs. Each variant has an independent warm SafeWIR verdict cache and an isolated XDG cache home. Before each process starts, the runner clears only its own `witchy/wasm` native cache directory, giving both binaries a cold Wasmtime compile. Compiler caches outside that directory remain warm. `bench_ns` brackets the VM workload; process wall time includes compilation, admission, and startup. Peak RSS describes the whole process, not only the VM heap. Where macOS reports retired instructions, elapsed cycles, and peak footprint, the runner records these whole-process measurements too. Raw output, samples, median/min/max/MAD, and paired bootstrap intervals remain in the output directory.

Add `--go /absolute/path/to/go` to compile the matching native fixtures before timing and cycle all six baseline, candidate, and Go execution orders. Twelve measured samples give each order twice, balancing both position and pair order. Packet and scalar fixtures use the same message counts and bounded capacity; the scalar Go fixture gains a kernel bracket around channel creation, sending, and consumption. The unchanged select fixture measures source-priority consumption after its producers finish. Generated Go sources, binary hashes, compiler version, validated outputs, and raw samples are retained. The report includes candidate/Go ratios and their paired bootstrap intervals; the ≤3× acceptance column passes only when the upper interval bound meets that target. A short smoke run checks the harness, but does not establish target performance.

Packet and scalar channels run at 1,000 and 10,000 messages. Select fan-in is the unchanged control. These are bounded CPU-only single-VM workloads; they do not establish I/O throughput, parallel scaling, or cross-VM ordering. Successful admitted-Wasm correctness fixtures also check heap reclamation, replay, mixed payloads, cancellation, scheduling, and fallback behavior. Allocation counts are unavailable through the canonical runtime’s current public statistics; zeros from missing legacy counters must not be interpreted as zero allocations. Hardware branch/cache counters are unavailable on the local macOS host.

The scheduler still constructs slot values, decodes typed payloads, and performs ownership repairs. This slice removes per-message continuation construction on eligible receivers; it does not eliminate producer continuations or all scheduler allocations. Compare optimized release builds only, and disclose control regressions alongside receive-loop wins.

## Cache policy and interpretation

The original prototype matrix accidentally compared a warm native-code baseline with a cold candidate. `Runtime::batch` configures Wasmtime’s cache under `$XDG_CACHE_HOME/witchy/wasm` (or the user cache directory). The CLI arms filesystem confinement before creating the runtime, so existing entries can be read, but new entries cannot be written without a matching filesystem grant. Repeating the sandbox process therefore does not warm an absent native entry. Original whole-process wall, RSS, and instruction figures are unsuitable for attributing a code regression.

Stage isolation measured baseline/prototype peak RSS around 200/210 MiB for source checking and 217/228 MiB for Wasm emission. A symbolized release sampling profile showed a native-cache hit for the baseline and Cranelift compilation, including register allocation, for the prototype. The supplied XDG directory is verified against both revisions’ runtime cache configuration; post-run file counts are an additional observation, not proof of cache hits or misses.

The current runner gives both revisions an empty native cache before every process. VM kernel time still excludes compilation, while process wall, retired instructions, and peak memory now include a matched cold native compile. This keeps confinement, SafeWIR admission, and VM-local scheduling unchanged. Native cached launches should be measured separately using explicitly populated caches; no cache performance is inferred from repeating confined launches.

The receiver implementation appends its private variants and dispatch arms after existing cases, retaining the existing variant tags. The one-time Step variant holds its fields in one tuple, limiting its contribution to the shared tagged-sum storage. The running Slot stays flat to avoid allocating a tuple on every message. Probe phases 0 and 2 share their identical code; the body, quiescence, completion, and cancellation round boundaries remain unchanged.

The original `full-matrix/` process metrics have asymmetric native-cache state. `final-matrix/` also cannot test its alleged std layout change, because release binaries embed std. Both are retained as diagnostic artifacts rather than current performance evidence. The corrected matrix uses saved release binaries, corresponding build snapshots, and explicit cache policy.

## Measured receiver slice

Twelve alternating pairs on Apple M4, macOS arm64, Rust beta 2026-10-03, and Wasmtime 49.0.2 use explicit cold native caches and warm admission verdicts. Baseline is the saved `0679eb85f` release compiler (compiler code unchanged by tooling-only `ad12fc0f9`); the candidate uses the packed entry Step and flat running Slot. Binary, fixture, harness, and embedded-std build-source hashes are retained with the samples.

| Workload | Baseline VM ms | Candidate VM ms | Speedup, paired 95% CI |
| --- | ---: | ---: | ---: |
| packet-1000 | 102.903 | 66.120 | 1.556× (1.399–1.648) |
| packet-10000 | 981.722 | 653.820 | 1.502× (1.422–1.556) |
| scalar-1000 | 95.047 | 61.589 | 1.543× (1.530–1.561) |
| scalar-10000 | 952.920 | 618.329 | 1.541× (1.502–1.611) |
| select_fanin | 181.594 | 192.566 | 0.943× (0.870–1.065) |

| Workload | Baseline process s | Candidate process s | Baseline peak MiB | Candidate peak MiB |
| --- | ---: | ---: | ---: | ---: |
| packet-1000 | 0.978 | 1.005 | 619.4 | 588.7 |
| packet-10000 | 1.841 | 1.551 | 620.6 | 593.0 |
| scalar-1000 | 0.902 | 0.888 | 615.9 | 589.7 |
| scalar-10000 | 1.798 | 1.450 | 619.1 | 583.7 |
| select_fanin | 1.607 | 1.798 | 671.5 | 715.3 |

The larger receive loops improve by roughly 33–35%. The select control’s VM median is about 6% slower, with its interval overlapping parity under host contention; this is not evidence of a select win or proof of zero overhead. Its whole-process retired-instruction median increases from 27.82 to 29.51 billion. The added scheduler form has a modest compile/startup and storage cost for other channel programs, rather than the originally inferred threefold process-memory cost. This bounded receiver optimization does not establish performance for general async workloads.

Raw evidence: `state/agents/async-state-machine-20261007/continuation/final-matrix/`. Full-slot packing was rejected: it allocated tuples on every transition and reduced receiver gains without a convincing overall control benefit. The one-time entry remains packed, and the hot running slot remains flat. Symbolized baseline/candidate profiles and the earlier cache-policy diagnostics are retained under the same continuation directory.
