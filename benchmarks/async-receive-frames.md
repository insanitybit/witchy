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

The runner uses macOS `/usr/bin/time -lp`. It saves binary, standard-library, fixture, and harness hashes, validates every checksum, runs two warmups, and alternates twelve measured baseline/candidate pairs. Each variant has an independent warm SafeWIR verdict cache. `bench_ns` brackets the VM workload; process wall time includes compilation, admission, and startup. Peak RSS describes the whole process, not only the VM heap. Where macOS reports retired instructions, elapsed cycles, and peak footprint, the runner records these whole-process measurements too. Raw output, samples, median/min/max/MAD, and paired bootstrap intervals remain in the output directory.

Packet and scalar channels run at 1,000 and 10,000 messages. Select fan-in is the unchanged control. These are bounded CPU-only single-VM workloads; they do not establish I/O throughput, parallel scaling, or cross-VM ordering. Successful admitted-Wasm correctness fixtures also check heap reclamation, replay, mixed payloads, cancellation, scheduling, and fallback behavior. Allocation counts are unavailable through the canonical runtime’s current public statistics; zeros from missing legacy counters must not be interpreted as zero allocations. Hardware branch/cache counters are unavailable on the local macOS host.

The scheduler still constructs slot values, decodes typed payloads, and performs ownership repairs. This slice removes per-message continuation construction on eligible receivers; it does not eliminate producer continuations or all scheduler allocations. Compare optimized release builds only, and disclose control regressions alongside receive-loop wins.

## Prototype outcome

Twelve alternating release pairs on Apple M4, macOS arm64, Rust beta 2026-10-03, and Wasmtime 49.0.2 compare baseline compiler `0679eb85f` against the prototype. Baseline compiler code is unchanged by `ad12fc0f9`, which only adds benchmark tooling. Saved binary and embedded-std source hashes identify the measured versions.

| Workload | Baseline VM ms | Prototype VM ms | Speedup, paired 95% CI |
| --- | ---: | ---: | ---: |
| packet-1000 | 96.710 | 61.878 | 1.563× (1.555–1.570) |
| packet-10000 | 984.786 | 629.926 | 1.563× (1.522–1.575) |
| scalar-1000 | 95.268 | 60.698 | 1.570× (1.557–1.578) |
| scalar-10000 | 945.681 | 600.629 | 1.574× (1.541–1.599) |
| select_fanin | 166.199 | 172.067 | 0.966× (0.959–1.008) |

Receive loops improve by roughly 36%. Select fan-in’s median VM time regresses by 3.5%, with this run’s interval overlapping parity. Its whole-process median increases from 1.187 s to 1.468 s, and median peak RSS increases from 212.5 MiB to 711.4 MiB. This is an unacceptable compile-path cost for a control that does not use the optimization; the prototype is not a landing recommendation.

On the same debug compiler, substituting the baseline standard library gives about 234 MiB peak RSS, while the prototype standard library gives about 728 MiB. Removing the new generic Step/API while retaining the new Slot still shows the increase. This localizes the cost to scheduler representation changes rather than establishing an allocation-count explanation. A sampling profile is available, but unsymbolized Wasm addresses limit attribution; further compiler profiling is needed before claiming the exact memory mechanism.

The valid release comparison is `state/agents/async-state-machine-20261007/full-matrix/`. The later `final-matrix/` directory is an invalid layout comparison: its snapshot arguments did not change the release binary’s embedded std. The original comparison uses the corresponding build snapshots correctly. The working prototype retains that measured std implementation; subsequent layout and slot experiments are preserved only as diagnostic artifacts.

Whole-process retired-instruction medians from the same raw timing outputs are 34.34 billion → 27.20 billion for packet-10,000, 33.64 billion → 26.49 billion for scalar-10,000, and 19.31 billion → 29.18 billion for select fan-in. These include compilation and execution, so they substantiate the control’s added total work without identifying guest-only instruction counts.
