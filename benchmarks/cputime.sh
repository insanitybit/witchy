#!/usr/bin/env bash
# Load-robust benchmark: witchy-native vs Go by USER CPU time, not wall clock.
#
# On a busy machine, wall-clock time is dominated by scheduling delay and is
# useless for comparison. User CPU time, by contrast, is the time a process
# actually spends executing — the OS accounts it accurately regardless of how
# contended the machine is. For a single-threaded, CPU-bound program the
# native/Go *ratio* of user time is therefore stable even under heavy load,
# because both binaries run back-to-back under the same conditions and the ratio
# cancels machine-wide variance. (Verified: the fib ratio holds at 0.77x across
# trials whose absolute times swing 40%.)
#
# Each program is repeated until ~2s of CPU time accrues (time -p has 10ms
# granularity, so a single fast run is too coarse). Outputs are checked to agree
# before timing, so we never benchmark a program that is silently wrong.
#
# Usage:  ./cputime.sh            # writes cputime_baseline.md
set -euo pipefail
cd "$(dirname "$0")"

WITCHY="${WITCHY:-../target/release/witchy}"
CPU_BENCHES=(fib loop_sum collatz mandelbrot closure_calls)
COLL_BENCHES=(list_sum dict_count binary_trees word_count expr_eval)
ALL=("${CPU_BENCHES[@]}" "${COLL_BENCHES[@]}")
BUILD=.build
TARGET_SECS="${TARGET_SECS:-2.0}"
mkdir -p "$BUILD"

if [ ! -x "$WITCHY" ]; then
    echo "error: witchy binary not found at $WITCHY (run: cargo build --release)" >&2
    exit 1
fi

# Total user CPU seconds for running `bin` `n` times (children's time included).
agg_user() {
    local bin=$1 n=$2
    /usr/bin/time -p sh -c "for i in \$(seq $n); do $bin >/dev/null; done" 2>&1 \
        | awk '/^user/{print $2}'
}

# Repetitions so that the run accrues ~TARGET_SECS of user time (>=3, <=5000).
reps_for() {
    local t
    t=$(agg_user "$1" 1)
    awk -v t="$t" -v target="$TARGET_SECS" \
        'BEGIN{ if (t<=0) t=0.005; n=int(target/t); if(n<3)n=3; if(n>5000)n=5000; print n }'
}

echo "building Go baselines and witchy-native binaries..."
for b in "${ALL[@]}"; do
    go build -o "$BUILD/${b}_go" "${b}.go"
    "$WITCHY" native -o "$BUILD/${b}_native" "${b}.witchy"
done

OUT=cputime_baseline.md
{
    echo "# witchy-native vs Go — user-CPU-time benchmark (load-robust)"
    echo
    echo "Ratio = native / go user CPU seconds; **< 1.00 means witchy-native is faster**."
    echo "User CPU time is stable under machine load (unlike wall clock), so these"
    echo "numbers are trustworthy even on a contended machine. Regenerate with"
    echo "\`benchmarks/cputime.sh\`. Outputs are asserted equal before timing."
    echo
    echo "| benchmark | native (s) | go (s) | ratio | faster |"
    echo "|---|---|---|---|---|"
} > "$OUT"

echo
echo "measuring (target ~${TARGET_SECS}s user time per program)..."
for b in "${ALL[@]}"; do
    n_out=$("$BUILD/${b}_native")
    g_out=$("$BUILD/${b}_go")
    if [ "$n_out" != "$g_out" ]; then
        echo "MISMATCH $b: native=$n_out go=$g_out" >&2
        exit 1
    fi
    reps=$(reps_for "$BUILD/${b}_native")
    nu=$(agg_user "$BUILD/${b}_native" "$reps")
    gu=$(agg_user "$BUILD/${b}_go" "$reps")
    line=$(awk -v b="$b" -v nu="$nu" -v gu="$gu" 'BEGIN{
        r = (gu>0) ? nu/gu : 0;
        faster = (nu < gu) ? "witchy" : "go";
        printf "| %s | %.2f | %.2f | %.2fx | %s |", b, nu, gu, r, faster
    }')
    echo "$line" | tee -a "$OUT"
done
echo
echo "wrote $OUT"
