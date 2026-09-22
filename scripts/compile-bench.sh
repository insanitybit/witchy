#!/usr/bin/env bash
# Controlled COMPILER benchmark — the one way to measure "how long does the
# compiler take on a program" so successive perf rounds compare like against
# like. `gate-bench.sh` is its sibling for gate wall time; this one measures
# `witchy` itself.
#
# WHY THIS EXISTS. On 2026-09-20/21 a perf session tuned exclusively against
# the async boundary test (an 11k-block generated executor). Six changes were
# measured there. Then one profile of a REAL program showed a completely
# different machine:
#
#     workload                     typeck   allocator
#     async executor (11k blocks)  10-15%     ~18%
#     gm_core.witchy (1,824 lines)    85%     ~70%
#
# The very next change, found only because a real program was profiled, was
# worth 17% -- larger than everything the async benchmark had produced. ONE
# BENCHMARK IS NOT A WORKLOAD. This script runs a spread on purpose.
#
# Every trap below was hit in practice during that session:
#   * COMMIT FIRST. The A/B swaps files with `git checkout <ref> -- <path>`
#     and restores with `git checkout HEAD -- <path>`; an uncommitted change
#     is silently destroyed and you measure a ref against itself.
#   * ALTERNATE IN ONE WINDOW and take the MIN. This box's load swings 10x
#     within seconds. A branch reading of 2.70s was briefly called a 30%
#     regression against a 2.05s master reading taken hours earlier; measured
#     back to back the same minute, they were 2.70 vs 2.73 -- neutral.
#   * NEVER touch the tree while a presubmit runs; it voids the receipt.
#   * A profile SHARE is not a lever. Ten ideas chosen from flame-profile
#     percentages measured as nothing; the wins came from counting one
#     specific quantity. Use this to confirm, not to hunt.
#
# Usage:
#   ./scripts/compile-bench.sh                     # time current build, all workloads
#   ./scripts/compile-bench.sh --against master    # alternating A/B vs a ref
#   ./scripts/compile-bench.sh --rounds 5 --only real
set -euo pipefail
cd "$(dirname "$0")/.."

rounds=3
against=""
only=""
while [ $# -gt 0 ]; do
    case "$1" in
        --against) against="${2:?--against needs a git ref}"; shift 2 ;;
        --rounds)  rounds="${2:?--rounds needs a count}"; shift 2 ;;
        --only)    only="${2:?--only needs a workload name}"; shift 2 ;;
        --help|-h) sed -n '2,/^set -euo pipefail$/{/^set -euo pipefail$/d; s/^# \{0,1\}//; p; }' "$0"; exit 0 ;;
        *) echo "compile-bench: unknown argument '$1'" >&2; exit 2 ;;
    esac
done

# name:path:description. Deliberately a SPREAD: a large real library, a
# mid-size stdlib module, a small corpus program, and the async executor the
# 2026-09 session over-fitted to. Add to this list rather than replacing it.
workloads=(
    "real:projects/grimoire/src/gm_core.witchy:1.8k-line real library (typeck-dominated)"
    "stdlib:std/json.witchy:mid-size stdlib module"
    "corpus:tests/rfc0165/corpus/025-aggregates_with_string_payloads_construct_match_and_propagate.witchy:small canonical corpus program"
    "async:examples/channels/src/channels.witchy:23-line async program (std executor dominates)"
)

binary="target/debug/witchy"
[ -x "$binary" ] || { echo "compile-bench: no $binary — run: ./scripts/bin/cargo build -p witchy" >&2; exit 1; }

cores="$(sysctl -n hw.ncpu 2>/dev/null || echo 0)"
load1() { sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}' || uptime | sed 's/.*averages*: //' | cut -d' ' -f1; }
warn_load() {
    local l; l="$(load1)"
    # `set -e` plus a `&&` chain whose test legitimately fails would abort the
    # whole script the moment the box is QUIET, which is the one case that
    # matters. Keep the result out of the function's exit status.
    if awk -v l="$l" -v c="$cores" 'BEGIN { exit !(c > 0 && l+0 > c*1.5) }'; then
        printf '\033[1;33mcompile-bench: load %s on %s cores — contention will dominate; treat results as indicative only\033[0m\n' "$l" "$cores"
    fi
    return 0
}

# One timed run: CPU seconds, which is far more robust to a loaded box than
# wall. The verdict store is disabled so the checker is actually exercised
# rather than replayed.
timed() { # timed <path>
    WITCHY_SAFEWIR_CHECK_CACHE=off RUST_MIN_STACK=16777216 \
        /usr/bin/time -p "$binary" check "$1" 2>&1 >/dev/null |
        awk '/^user/{u=$2} /^sys/{s=$2} END{printf "%.2f", u+s}'
}

best_of() { # best_of <path> <rounds>
    local best="" t i=0
    while [ "$i" -lt "$2" ]; do
        t="$(timed "$1")"
        if [ -z "$best" ] || awk -v a="$t" -v b="$best" 'BEGIN{exit !(a+0 < b+0)}'; then best="$t"; fi
        i=$((i + 1))
    done
    printf '%s' "$best"
}

warn_load
if [ -n "$against" ]; then
    [ -z "$(git status --porcelain)" ] || {
        echo "compile-bench: commit first — the A/B swap would destroy uncommitted work (see header)" >&2
        exit 2
    }
    git rev-parse --verify --quiet "$against" >/dev/null || { echo "compile-bench: no such ref '$against'" >&2; exit 2; }
fi

printf '%-8s %-46s %10s' "workload" "description" "cpu_s"
[ -n "$against" ] && printf ' %10s %8s' "$against" "delta"
printf '\n'

for entry in "${workloads[@]}"; do
    name="${entry%%:*}"; rest="${entry#*:}"; path="${rest%%:*}"; desc="${rest#*:}"
    [ -n "$only" ] && [ "$only" != "$name" ] && continue
    [ -f "$path" ] || { printf '%-8s %-46s %10s\n' "$name" "$desc" "MISSING"; continue; }
    here="$(best_of "$path" "$rounds")"
    if [ -z "$against" ]; then
        printf '%-8s %-46s %10s\n' "$name" "$desc" "$here"
        continue
    fi
    # Alternating A/B in one window: rebuild the ref, measure, restore, measure.
    git stash list >/dev/null
    git checkout -q "$against" -- crates src 2>/dev/null || true
    ./scripts/bin/cargo build -q -p witchy 2>/dev/null || true
    there="$(best_of "$path" "$rounds")"
    git checkout -q HEAD -- crates src
    ./scripts/bin/cargo build -q -p witchy 2>/dev/null || true
    delta="$(awk -v a="$here" -v b="$there" 'BEGIN{ if (b+0 == 0) print "n/a"; else printf "%+.1f%%", 100*(a-b)/b }')"
    printf '%-8s %-46s %10s %10s %8s\n' "$name" "$desc" "$here" "$there" "$delta"
done

printf '\nload now %s on %s cores. CPU seconds, best of %s.\n' "$(load1)" "$cores" "$rounds"
