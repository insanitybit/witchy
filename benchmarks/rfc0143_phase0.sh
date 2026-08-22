#!/usr/bin/env bash
# RFC-0143 Phase-0 evidence harness.
#
# Run this script once for each compiler configuration (for example, the
# current scalar control and the Swiss branch), then compare the raw TSV files
# with the same host and input.  Compilation and cache setup are outside the
# timed kernel: the workload itself emits bench_ns=... which is the value
# recorded here.
set -euo pipefail
cd "$(dirname "$0")/.."

WITCHY="${WITCHY:-target/release/witchy}"
RUNS="${RUNS:-8}"
WARMUP="${WARMUP:-2}"
LABEL="${LABEL:-rfc0143}"
OUT="${OUT:-benchmarks/.build/rfc0143-${LABEL}-$(date +%Y%m%d-%H%M%S).tsv}"

if [[ ! -x "$WITCHY" ]]; then
  echo "error: witchy binary not found at $WITCHY (build it first)" >&2
  exit 1
fi
mkdir -p "$(dirname "$OUT")"

cat >"$OUT" <<EOF
# rfc=0143 label=$LABEL revision=$(git rev-parse HEAD)
# host=$(uname -srvm)
# compiler=$("$WITCHY" --version 2>/dev/null || true)
# runs=$RUNS warmup=$WARMUP
workload,sample,bench_ns
EOF

kernel_ns() { grep '^bench_ns=' | head -1 | cut -d= -f2 || true; }
for workload in dict_count word_count knucleotide; do
  source="benchmarks/${workload}.witchy"
  for _ in $(seq 1 "$WARMUP"); do
    "$WITCHY" sandbox "$source" >/dev/null 2>&1 || true
  done
  for sample in $(seq 1 "$RUNS"); do
    ns=$("$WITCHY" sandbox "$source" 2>/dev/null | kernel_ns)
    [[ -n "$ns" ]] || { echo "missing bench_ns for $workload" >&2; exit 1; }
    printf '%s,%s,%s\n' "$workload" "$sample" "$ns" | tee -a "$OUT"
  done
done
echo "wrote $OUT" >&2
