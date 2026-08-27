#!/usr/bin/env bash
# RFC-0153 matched matrix for Coven's existing all_target_digests consumer.
# Each invocation gets a fresh Dir root: fixture construction is outside the
# program clock and cannot reuse records from the opposing binary.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

CONTROL_WITCHY=${CONTROL_WITCHY:?set CONTROL_WITCHY}
CANDIDATE_WITCHY=${CANDIDATE_WITCHY:?set CANDIDATE_WITCHY}
CONTROL_OPT=${CONTROL_OPT:-all}
CANDIDATE_OPT=${CANDIDATE_OPT:-all}
SOURCE=${SOURCE:-projects/coven/src/rfc0153_coven_snapshot.witchy}
OUT_DIR=${OUT_DIR:-benchmarks/.build/rfc0153-coven-$(date -u +%Y%m%d-%H%M%S)}
RUNS=${RUNS:-12}
WARMUP=${WARMUP:-2}

[[ $RUNS == 12 && $WARMUP == 2 ]] || { echo 'RFC-0153 requires 2 warmups and 12 samples' >&2; exit 2; }
[[ -x $CONTROL_WITCHY && -x $CANDIDATE_WITCHY && -f $SOURCE ]] || { echo 'missing binary or source' >&2; exit 2; }
mkdir -p "$OUT_DIR"
printf 'rfc=0153\nrevision=%s\nhost=%s\nsource=%s\nsource_sha256=%s\ncontrol_sha256=%s\ncandidate_sha256=%s\ncontrol_opt=%s\ncandidate_opt=%s\nruns=%s\nwarmup=%s\n' \
    "$(git rev-parse HEAD)" "$(uname -srvm)" "$SOURCE" \
    "$(shasum -a 256 "$SOURCE" | awk '{print $1}')" \
    "$(shasum -a 256 "$CONTROL_WITCHY" | awk '{print $1}')" \
    "$(shasum -a 256 "$CANDIDATE_WITCHY" | awk '{print $1}')" "$CONTROL_OPT" "$CANDIDATE_OPT" \
    "$RUNS" "$WARMUP" > "$OUT_DIR/metadata.txt"

run() {
    local binary=$1 opt=$2 output root
    root=$(mktemp -d /tmp/rfc0153-coven-root.XXXXXX)
    if ! output=$(WITCHY_OPT="$opt" "$binary" sandbox --dir "$root" "$SOURCE" 2>/dev/null); then
        rm -rf "$root"
        return 1
    fi
    rm -rf "$root"
    printf '%s\n' "$output"
}

kernel_ns() { awk -F= '/^bench_ns=/{ print $2; exit }'; }
result() { grep -v '^bench_ns=' || true; }
sha256() { shasum -a 256 | awk '{print $1}'; }

printf 'mode,sample,kernel_ns\n' > "$OUT_DIR/timing.csv"
control_result=$(run "$CONTROL_WITCHY" "$CONTROL_OPT" | result)
candidate_result=$(run "$CANDIDATE_WITCHY" "$CANDIDATE_OPT" | result)
[[ -n $control_result && $control_result == "$candidate_result" ]] || { echo 'correctness mismatch' >&2; exit 1; }
printf 'source_sha256=%s\ncontrol_output_sha256=%s\ncandidate_output_sha256=%s\n' \
    "$(shasum -a 256 "$SOURCE" | awk '{print $1}')" \
    "$(printf '%s' "$control_result" | sha256)" \
    "$(printf '%s' "$candidate_result" | sha256)" > "$OUT_DIR/correctness.txt"

for mode in control candidate; do
    if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; opt=$CONTROL_OPT; else binary=$CANDIDATE_WITCHY; opt=$CANDIDATE_OPT; fi
    for _ in $(seq 1 "$WARMUP"); do run "$binary" "$opt" >/dev/null; done
done
for sample in $(seq 1 "$RUNS"); do
    if (( sample % 2 )); then modes='control candidate'; else modes='candidate control'; fi
    for mode in $modes; do
        if [[ $mode == control ]]; then binary=$CONTROL_WITCHY; opt=$CONTROL_OPT; else binary=$CANDIDATE_WITCHY; opt=$CANDIDATE_OPT; fi
        ns=$(run "$binary" "$opt" | kernel_ns)
        [[ -n $ns ]] || { echo 'missing kernel clock' >&2; exit 1; }
        printf '%s,%s,%s\n' "$mode" "$sample" "$ns" >> "$OUT_DIR/timing.csv"
    done
done

echo "wrote RFC-0153 Coven matrix artifact: $OUT_DIR" >&2
