# Atomic reference-list replacement performance

Indexed replacement in a complete, uniquely owned reference list now updates the existing slot instead of rebuilding the list. SafeWIR requires the sole unique RC parent token, initialized owned storage within logical length, physical bounds for the old-slot read, and no unsupported affine child plan or self-owned edge. The backend retains a shared replacement before publication, then releases the displaced edge; unique replacements transfer their token. Shared and unsupported lists retain their existing copy path.

A matched ARM64 macOS release comparison uses parent `b2e2c6ae0` and candidate `681031313`, with saved binaries and standard-library snapshots. A synchronous four-element `List(List(Int))` workload performs 16,384 replacements, returns independently checked checksum 65526, and improves from 0.800 ms to 0.119 ms: 6.74× faster, with a paired bootstrap 95% interval of 3.74–6.87× over twelve AB/BA pairs and two warmups. Three counted runs per binary agree: allocations fall from 98,315 to 32,779, retains from 49,156 to 4, and releases from 81,939 to 32,787.

The unchanged nine-row async matrix at 1,024 messages, twelve balanced rounds, Go 1.26.5, and GOMAXPROCS=1 shows clear improvements on all four slow rows:

| Workload | Speedup | 95% interval | Candidate / Go |
| --- | ---: | --- | ---: |
| String packet | 1.32× | 1.21–1.39× | 18.58× |
| Four producers, one consumer | 1.29× | 1.10–1.34× | 148.08× |
| Sixteen producers, one consumer | 1.39× | 1.15–1.49× | 235.05× |
| Four producers, four consumers | 1.34× | 1.25–1.41× | 123.79× |

The five fixed-packet rows show no clear before/after timing change and retain confidence-bound ratios below 3× Go. On the four-producer, one-consumer row, three identical counted runs per binary show allocations falling from 136,290 to 103,744, retains from 226,169 to 180,952, and releases from 231,128 to 195,869. These counters cover whole guest linear-memory execution, including setup and output, and exclude host and Wasm GC allocations. Timings use uninstrumented binaries; counted timings are not performance evidence. Host noise is visible in the raw timing samples.

All 76 native codegen tests and 139 verifier/backend unit tests passed, including child/parent snapshots, self-replacement, heap cleanup, logical and physical bounds, and spare-capacity refusal. Full presubmit, broader admission verification, and async scaling qualification remain outstanding. The async ≤3× matched Go objective is not achieved. The held transport experiment is excluded and remains unmerged.

Raw source/binary fingerprints, three repeated counts, paired timings, independent checksums, and admission logs are retained under `state/agents/general-reference-slot-20261009/`. The standalone allocation/timing fixture is retained there as `nested-list-replace.witchy`; the unchanged async matrix is `matrix-1024/results.json`.
