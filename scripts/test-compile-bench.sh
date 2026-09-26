#!/usr/bin/env bash
# Exercise benchmark control flow with tiny stand-in executables, no compiler build.
set -euo pipefail
cd "$(dirname "$0")/.."
scratch="$(mktemp -d "${TMPDIR:-/tmp}/witchy-compile-bench-test.XXXXXX")"
trap 'rm -r "$scratch"' EXIT
before="$(git diff HEAD -- crates src)"
bench=(bash scripts/compile-bench.sh --rounds 2 --only corpus)
"${bench[@]}" --binary /bin/echo --against-binary /bin/echo --output-dir "$scratch/pass" > "$scratch/pass.log"
test "$(awk 'END{print NR}' "$scratch/pass/samples.tsv")" -eq 5
test -f "$scratch/pass/corpus-candidate-0.stdout"
test -f "$scratch/pass/corpus-baseline-2.stdout"
expect_failure() {
    local expected="$1"; shift
    if "$@" > "$scratch/failure.log" 2>&1; then
        echo "benchmark unexpectedly succeeded: $*" >&2; exit 1
    fi
    grep -Fq "$expected" "$scratch/failure.log"
}
expect_failure 'output mismatch' "${bench[@]}" --binary /bin/echo --against-binary /usr/bin/true --output-dir "$scratch/mismatch"
expect_failure 'candidate failed' "${bench[@]}" --binary /usr/bin/false --output-dir "$scratch/failed"
test "$(awk 'END{print NR}' "$scratch/failed/samples.tsv")" -eq 1
expect_failure 'baseline failed' "${bench[@]}" --binary /bin/echo --against-binary /usr/bin/false --output-dir "$scratch/baseline-failed"
expect_failure 'source-swapping --against was removed' "${bench[@]}" --against HEAD
expect_failure 'positive integer' "${bench[@]}" --rounds 00
expect_failure 'unknown workload' "${bench[@]}" --only typo
expect_failure 'output directory must be new' "${bench[@]}" --binary /bin/echo --output-dir "$scratch/pass"
test "$before" = "$(git diff HEAD -- crates src)"
echo 'compile benchmark contracts passed'
