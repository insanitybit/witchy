# RFC-0087 RFC-0051 performance preservation report

Date: 2026-07-16

Compiler source baseline: canonical `master` at `714df0a5`. The release binary
was built from `92ce6bf9`; `git diff 92ce6bf9..714df0a5 -- Cargo.toml src crates
std benchmarks` is empty, so the measured compiler and corpus are source-identical
to that master.

Platform: macOS 26.3.1, arm64.

Equivalent reproduction command from the worktree root:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  taskpolicy -c utility cargo build --release --bin witchy

./scripts/merge-queue.sh with-lock -- \
  env RUNS=3 WARMUP=1 TIMEOUT_SECONDS=90 \
  WITCHY=../target/release/witchy \
  OUTPUT=.build/rfc0087-inplace-current.tsv \
  taskpolicy -c utility \
  ./benchmarks/rfc0087_inplace_gate.sh
```

The shared gate lock excluded overlapping builds and gates. Each numeric row is
the best of three kernel-clock samples after one warmup. `WITCHY_OPT=all` is the
shipping optimized configuration; `WITCHY_OPT=-inplace` is the supported
forced-copy oracle. After an initial load-affected pass, two additional locked
runs placed the three threshold kernels within 1% of each other. The table uses
the final repeat.

| Benchmark | Optimized ns | Forced-copy | Forced / optimized |
| --- | ---: | ---: | ---: |
| `word_count` | 74,312,291 | expected memory trap | resource cliff |
| `dict_count` | 28,062,250 | expected memory trap | resource cliff |
| `list_sum` | 8,953,667 | expected memory trap | resource cliff |
| `knucleotide` | 38,729,500 | expected memory trap | resource cliff |
| `list_index` | 5,180,333 | 13,823,792 ns | 2.669x |
| `binary_trees` | 65,481,375 | 90,716,125 ns | 1.385x |
| `expr_eval` | 12,731,000 | 16,973,542 ns | 1.333x |

All seven optimized kernels completed. The four RFC-0051 memory-cliff kernels
failed only under forced-copy with the runtime's bounded-memory manifestation,
`wasm trap: out of bounds memory access`. The three numeric differentials exceed
the harness firing margins (1.10x for `list_index`, 1.05x for `binary_trees` and
`expr_eval`). Optimized output matched forced-copy output whenever forced-copy
completed.

[`benchmarks/rfc0087_inplace_reference.tsv`](../benchmarks/rfc0087_inplace_reference.tsv)
is the checked-in current reference. Subsequent runs reject an optimized timing
more than 5% slower for `list_index`, `binary_trees`, or `expr_eval`, while also
re-proving optimized completion, output parity, expected cliff classification,
and the forced-copy firing margin for all seven kernels.

A separate default-policy validation against that frozen reference passed with
5,142,041 ns / 65,881,708 ns / 12,540,459 ns for the three threshold kernels
and 2.624x / 1.248x / 1.357x forced-copy ratios.

This evidence preserves the existing general `inplace` paths. It adds no
per-method fast path and no new `*_cap` helper.
