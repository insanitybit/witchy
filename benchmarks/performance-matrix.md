# Matched performance matrix

Compare saved pre-change and current Witchy binaries with the saved Go binaries
from a full `bench.sh --full` run. The runner covers the 21 standard workloads
and the known scalar callback at 200,000, two million, and twenty million calls.
The callback's Go counterpart computes the same result using a named function
passed through a helper.

```sh
python3 scripts/performance-matrix.py \
    --baseline /path/to/before --current /path/to/current \
    --baseline-revision BEFORE_SHA --current-revision CURRENT_SHA \
    --go-artifacts /path/to/full-suite/raw \
    --output state/performance-matrix/run --runs 12
python3 scripts/render-performance-matrix.py \
    state/performance-matrix/run/matrix.json \
    --output state/performance-matrix/run/report.md
```

Use `--only loop_sum scalar_callback_200000` and a fresh output directory for a
bounded smoke run. The default twelve rounds cover all six variant orders
twice. Two warmups per variant precede measurement. Every measured result must
match the saved Go result or the callback's independently calculated checksum.
Each row retains raw samples, stderr, executable and input digests, variability,
and paired bootstrap intervals.

The runner uses the same current standard library for both Witchy executables,
`WITCHY_OPT=all`, and a warm worktree-local verdict cache. Existing Go executables
must have been built from the matching current `.go` files. Peak RSS and CPU
come from macOS `/usr/bin/time -lp`; CPU resolution is 0.01 seconds. The wall
clock includes that wrapper and output capture. Go's wall clock starts with a
precompiled binary; Witchy's includes source compilation and JIT. Compare
kernel clocks for execution performance.

The Wasm code-section comparison counts changed function bodies without relying
on a mutable compiler probe. Unchanged bytes demonstrate unchanged generated
code for that row; they are not dynamic hit counts or a proof of semantic
equivalence. Preserve the earlier admitted-Wasm conformance evidence separately.

Run the measurement behind the merge queue's `with-lock` when a serialized gate
could overlap. This protects against full gates, not every source of host
contention. Preserve raw observations outside Git. The standard suite and the
targeted mechanism should remain separate groups in reports.
