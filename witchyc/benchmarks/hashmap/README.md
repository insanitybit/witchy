# Mutable HashMap comparison

Build the baseline compiler at `c662a6a9` and the candidate in separate worktrees
with MBX. Each compiler embeds its own standard library. Run the harness from
the candidate checkout so both compilers receive identical benchmark sources.
Use the same Wasmtime host, proof binding and proof cache for both compilers.

```sh
python3 compare.py --baseline "$BASELINE" --candidate "$CANDIDATE" --target wasm --host "$HOST" --runs 31
python3 compare.py --baseline "$BASELINE" --candidate "$CANDIDATE" --target macos-arm64 --runs 31
```

The harness checks output on every run, discards one warmup per binary and
alternates which binary runs first within a pair. Its paired ratio divides
candidate elapsed time by baseline elapsed time. End-to-end elapsed time
includes process launch, Wasm startup where applicable, execution and teardown.
The Wasm host also reports startup and execution separately.

| Workload | Operations and distribution |
| --- | --- |
| map | A million integer insert-or-update operations and a million hit/miss lookups |
| words | Count repeated borrowed spans in a table of owned strings |
| sparse | At one-eighth load, mutate hits, probe absent keys and remove every entry |
| dense | At nine-tenths load, successful random lookups followed by misses |
| clustered | Long runs with shared starting slots and several fingerprint values |
| same-hash | Distinct keys with identical full hashes |
| churn | Repeated deletion, cluster repair and replacement insertion |

For the final confirmation, treat an end-to-end slowdown above 5% as material.
Compare the median pair and its sign interval on each workload and target.
If the upper end exceeds 1.05, report the uncertainty and investigate before
landing. Empty-map teardown and in-process timings can diagnose a regression.
The landing comparison uses the complete workloads above.


The mutable map uses vectorized fingerprinting, one of the lookup schemes
evaluated by Böther et al. Witchy keeps the entry buffer's linear probe order
and checks the first vacancy when returning a miss. The clustered and
identical-hash cases measure how much the filter buys when fingerprints can
or cannot eliminate candidates.
[Analyzing Vectorized Hash Tables Across CPU Architectures (PVLDB 2023)](https://www.vldb.org/pvldb/vol16/p2755-bother.pdf)
