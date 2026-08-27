#!/usr/bin/env bash
# RFC-0153 compiler-cost and emitted-Wasm corpus comparison.
#
# This is deliberately separate from the kernel promotion matrix: it measures
# the compiler binary, not execution.  It compiles every benchmark source with
# counterbalanced control/candidate ordering, retains every raw duration and
# emitted module, and rejects the RFC limits (Wasm <6%, compile time <8%).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

CONTROL_WITCHY=${CONTROL_WITCHY:-target/release/witchy}
CANDIDATE_WITCHY=${CANDIDATE_WITCHY:-$CONTROL_WITCHY}
LABEL=${LABEL:-rfc0153-impact}
OUT_DIR=${OUT_DIR:-benchmarks/.build/rfc0153-${LABEL}-$(date -u +%Y%m%d-%H%M%S)}
RUNS=${RUNS:-5}
WARMUP=${WARMUP:-1}
SOURCES=${SOURCES:-"$(printf '%s ' benchmarks/*.witchy)"}

[[ $RUNS =~ ^[1-9][0-9]*$ ]] || { echo 'RUNS must be positive' >&2; exit 2; }
[[ $WARMUP =~ ^[0-9]+$ ]] || { echo 'WARMUP must be non-negative' >&2; exit 2; }
for binary in "$CONTROL_WITCHY" "$CANDIDATE_WITCHY"; do
    [[ -x $binary ]] || { echo "missing executable: $binary" >&2; exit 2; }
done

mkdir -p "$OUT_DIR/modules" "$OUT_DIR/timing"
printf '%s\n' \
    'rfc=0153' "label=$LABEL" "revision=$(git rev-parse HEAD)" \
    "host=$(uname -srvm)" "runs=$RUNS" "warmup=$WARMUP" \
    "control_sha256=$(shasum -a 256 "$CONTROL_WITCHY" | awk '{print $1}')" \
    "candidate_sha256=$(shasum -a 256 "$CANDIDATE_WITCHY" | awk '{print $1}')" \
    >"$OUT_DIR/metadata.txt"
printf 'source,source_sha256,mode,sample,compile_seconds,wasm_bytes,wasm_sha256\n' >"$OUT_DIR/raw.csv"

compile_one() {
    local mode=$1 binary=$2 source=$3 sample=$4 stem time_file wasm seconds bytes digest
    stem=$(basename "${source%.witchy}")
    time_file="$OUT_DIR/timing/${stem}.${mode}.${sample}.txt"
    wasm="$OUT_DIR/modules/${stem}.${mode}.${sample}.wasm"
    TIMEFORMAT='%R'
    { time "$binary" emit-wasm "$source" -o "$wasm" >/dev/null; } 2>"$time_file"
    seconds=$(tail -n 1 "$time_file")
    [[ $seconds =~ ^[0-9]+([.][0-9]+)?$ ]] || {
        echo "could not read compiler duration from $time_file" >&2; exit 1;
    }
    bytes=$(stat -f '%z' "$wasm")
    digest=$(shasum -a 256 "$wasm" | awk '{print $1}')
    printf '%s,%s,%s,%s,%s,%s,%s\n' \
        "$source" "$(shasum -a 256 "$source" | awk '{print $1}')" "$mode" "$sample" "$seconds" "$bytes" "$digest" \
        >>"$OUT_DIR/raw.csv"
}

for source in $SOURCES; do
    [[ -f $source ]] || { echo "missing source: $source" >&2; exit 2; }
    for mode in control candidate; do
        if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; else binary=$CANDIDATE_WITCHY; fi
        for warmup in $(seq 1 "$WARMUP"); do
            compile_one "$mode" "$binary" "$source" "warmup-${warmup}"
        done
    done
    for sample in $(seq 1 "$RUNS"); do
        if (( sample % 2 )); then order='control candidate'; else order='candidate control'; fi
        for mode in $order; do
            if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; else binary=$CANDIDATE_WITCHY; fi
            compile_one "$mode" "$binary" "$source" "$sample"
        done
    done
done

python3 - "$OUT_DIR/raw.csv" "$OUT_DIR/summary.json" "$RUNS" <<'PY'
import csv, json, statistics, sys

raw_path, summary_path, runs = sys.argv[1:]
rows = list(csv.DictReader(open(raw_path, newline='')))
active = [row for row in rows if row['sample'].isdigit() and int(row['sample']) <= int(runs)]
summary = {'schema': 1, 'sources': {}, 'totals': {}}
for source in sorted({row['source'] for row in active}):
    summary['sources'][source] = {}
    for mode in ('control', 'candidate'):
        entries = [row for row in active if row['source'] == source and row['mode'] == mode]
        sizes = {int(row['wasm_bytes']) for row in entries}
        if len(sizes) != 1:
            raise SystemExit(f'nondeterministic emitted Wasm size for {source}/{mode}: {sorted(sizes)}')
        digests = sorted({row['wasm_sha256'] for row in entries})
        if len(digests) != 1:
            raise SystemExit(f'nondeterministic emitted Wasm bytes for {source}/{mode}: {digests}')
        summary['sources'][source][mode] = {
            'median_compile_seconds': statistics.median(float(row['compile_seconds']) for row in entries),
            'wasm_bytes': sizes.pop(),
            'wasm_sha256': digests[0],
        }
for mode in ('control', 'candidate'):
    entries = [row for row in active if row['mode'] == mode]
    totals = []
    for sample in range(1, int(runs) + 1):
        sample_rows = [row for row in entries if int(row['sample']) == sample]
        totals.append(sum(float(row['compile_seconds']) for row in sample_rows))
    summary['totals'][mode] = {
        'median_compile_seconds': statistics.median(totals),
        'wasm_bytes': sum(value[mode]['wasm_bytes'] for value in summary['sources'].values()),
    }
control, candidate = summary['totals']['control'], summary['totals']['candidate']
summary['totals']['wasm_ratio'] = candidate['wasm_bytes'] / control['wasm_bytes']
summary['totals']['compile_time_ratio'] = candidate['median_compile_seconds'] / control['median_compile_seconds']
summary['passes'] = summary['totals']['wasm_ratio'] < 1.06 and summary['totals']['compile_time_ratio'] < 1.08
with open(summary_path, 'w') as output:
    json.dump(summary, output, indent=2, sort_keys=True)
    output.write('\n')
print(json.dumps(summary['totals'], indent=2, sort_keys=True))
if not summary['passes']:
    raise SystemExit('RFC-0153 impact limits exceeded')
PY

echo "wrote RFC-0153 impact artifact: $OUT_DIR" >&2
