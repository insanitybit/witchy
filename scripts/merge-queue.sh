#!/usr/bin/env bash
# Merge/gate coordinator for concurrent agents (see CLAUDE.md "Concurrent
# agents"). It fixes the two expensive failure modes of parallel work:
# duplicated full gates running at once (they stretch each other's long-tail
# e2e tests, and the publish e2e is load-flaky), and a merge landing while a
# full gate runs (which invalidates that gate and forces a rebase + rerun).
#
# The protocol:
#   * Agents work in isolated worktrees and run only FOCUSED tests there
#     (a check.sh shard: --fast / --examples).
#   * When a branch is ready, the agent runs:  scripts/merge-queue.sh submit <branch>
#   * ONE coordinator session runs:            scripts/merge-queue.sh run
#     It takes branches FIFO, rebases each onto current master in a dedicated
#     warm gate worktree (.claude/worktrees/merge-gate), runs the full gate
#     there under the gate lock, and on green fast-forwards master. If master
#     moved while a gate ran by a code change, the candidate is re-rebased and
#     re-gated instead of merging a stale validation. A master tip that moved
#     only by docs-safe paths does not invalidate an already-green code gate:
#     the coordinator replays the green sha onto the new tip and fast-forwards.
#     Docs-safe landings do not wait on gate.lock. A red/timed-out candidate is
#     journaled and dropped; the queue CONTINUES to the next branch (resubmit
#     after fixing).
#   * Anything else heavyweight (an ad-hoc full suite) should share the same
#     lock:  scripts/merge-queue.sh with-lock -- ./scripts/check.sh --fast
#
# Stall resistance: the gate runs in its own process group under a monitor.
# NEXTEST_STATUS_LEVEL=pass makes nextest stream each finished test, but Cargo's
# final compile line to nextest's first result can still be quiet for minutes.
# check.sh emits bounded stage heartbeats across that window (eight in the
# serialized gate). Cargo
# incremental output is disabled for the gate worktree: it is repeatedly rebased
# across unrelated branches, so that state has little reuse value and otherwise
# grows without bound between gates. If the log goes quiet for
# MERGE_QUEUE_STALL_TIMEOUT seconds (default 600), or stays busy without real
# progress for MERGE_QUEUE_BUSY_SILENCE_MAX, the process group is killed. An
# optional MERGE_QUEUE_GATE_TIMEOUT adds an emergency whole-gate ceiling; it is
# disabled by default because a progressing cold gate is not a semantic red.
# Separately, MERGE_QUEUE_WORKSPACE_TEST_BUDGET (default 1800s) is a hard
# wall-clock on the workspace-test stage starting at `==> [N] tests`. PASS
# lines and heartbeat pulses do not reset it: a starved-but-chatty nextest
# that would otherwise crawl for an hour is a timeout, not a green. 0 disables
# the budget. Raised 600s -> 1800s on 2026-08-26: the tests stage measures
# p50 226s / p90 694s over 1,051 gates, so a 600s ceiling sat BELOW the p90 of
# the very thing it bounds and killed roughly a tenth of legitimate gates --
# four branches hit it in one six-hour window. 1800s still catches the
# hour-long crawl this exists for. Timed-out candidates are journaled, the lock is released, and
# the queue moves on. Logs are always preserved under state/merge-queue/logs/.
# The bounded nextest list wrapper also records genuine discovery-wave progress
# in a coordinator-owned sidecar; synthetic human heartbeats do not count as
# liveness.
#
# State is machine-readable and lives under gitignored state/merge-queue/
# IN THE MAIN WORKTREE (each worktree has its own state/, so state written
# elsewhere would be invisible to the coordinator):
#   queue/*.json    one pending submission per file (FIFO by filename)
#   journal.jsonl   append-only events: submitted/merged/already_merged/red/
#                   timeout/conflict/requeued/blocked/dropped/rebaselined/
#                   evicted (red+timeout carry the log path)
#   logs/           full gate output per attempt (check.sh stage markers carry
#                   t+<seconds> offsets, so per-stage timing is in every log)
#   coordinator.lock/ lifetime singleton for the persistent coordinator loop
#   gate.lock/      the lock: pid + what + branch + log + started epoch, plus
#                   gate_pgid while a full gate process group is active
#                   (stale locks — dead pid — are stolen)
#
#   scripts/merge-queue.sh submit [--front] <branch> [note]
#                                                   enqueue a local branch (warns if the
#                                                   diff overlaps another queued branch).
#                                                   --front puts it at the HEAD of the
#                                                   queue (urgent fixes; use sparingly)
#   scripts/merge-queue.sh wait <branch> [secs]     block until the branch reaches a
#                                                   terminal journal event (merged/
#                                                   already_merged/red/timeout/
#                                                   conflict/blocked/dropped),
#                                                   print it as JSON; exit 0 iff merged.
#                                                   Default timeout 3600s
#   scripts/merge-queue.sh status                   queue + in-flight gate + recent journal (JSON).
#                                                   `blocked_on` is non-null when the main checkout
#                                                   cannot land a gated candidate (tracked edits on
#                                                   master), which otherwise looks like an idle queue.
#   scripts/merge-queue.sh drop <branch> <reason>   retire a pending submission without deleting
#                                                   its branch; the reason is journaled and
#                                                   dependents remain blocked until resubmitted
#   scripts/merge-queue.sh doctor                   human health check: coordinator alive?
#                                                   lock stale? current stage? log fresh?
#   scripts/merge-queue.sh run [--once]             coordinator loop (--once: drain and exit)
#   scripts/merge-queue.sh daemon                   start the coordinator in a detached session,
#                                                   surviving the launching session; log →
#                                                   state/merge-queue/coordinator.log; stop
#                                                   with: kill $(cat .../coordinator.pid)
#   scripts/merge-queue.sh migrate-state            one-time guarded scratch/ → state/ cutover
#   scripts/merge-queue.sh with-lock -- <cmd...>    run any command under the gate lock
#   scripts/merge-queue.sh sweep                    remove worktrees whose branch this
#                                                   queue MERGED (journal-verified) and
#                                                   whose tree is clean — each holds a
#                                                   multi-GB target/, so disk fills fast.
#                                                   Also -d's those merged branches. The
#                                                   coordinator sweeps after every merge;
#                                                   worktree-status.sh reports what sweep
#                                                   can't judge (abandoned/dirty trees).
#
# The gate command defaults to `./scripts/check.sh` (the push gate minus --full);
# override with MERGE_QUEUE_GATE_CMD (e.g. "./scripts/check.sh --full").
set -euo pipefail

# All state lives under the MAIN worktree, no matter which worktree this script
# is invoked from. MERGE_QUEUE_STATE_DIR / MERGE_QUEUE_GATE_WT exist so tests
# can run against throwaway state without touching the live queue.
here="$(cd "$(dirname "$0")/.." && pwd)"
if [ -n "${MERGE_QUEUE_TEST_ROOT:-}" ]; then
    [ "${MERGE_QUEUE_ALLOW_TEST_ROOT:-0}" = 1 ] || {
        printf 'merge-queue: MERGE_QUEUE_TEST_ROOT requires MERGE_QUEUE_ALLOW_TEST_ROOT=1\n' >&2
        exit 2
    }
    root="$MERGE_QUEUE_TEST_ROOT"
else
    # Do not use `head -1` here: with pipefail it can SIGPIPE `git worktree
    # list` once the shared checkout has enough worktrees to fill the pipe,
    # making every queue command exit 141 before it reads its state.
    root="$(git -C "$here" worktree list --porcelain | sed -n '1s/^worktree //p')"
fi
. "$here/scripts/state-paths.sh"
[ ! -f "$here/scripts/worktree-pool.sh" ] || . "$here/scripts/worktree-pool.sh"
qdir="$(witchy_merge_queue_state_dir "$root")"
queue_dir="$qdir/queue"
changes_dir="$qdir/changes"
change_lock="$qdir/change.lock"
journal="$qdir/journal.jsonl"
logs="$qdir/logs"
lock="$qdir/gate.lock"
# Written by the coordinator while it WAITS for the gate lock, so ad-hoc
# `with-lock` callers queue behind it instead of barging. Measured 2026-08-27:
# coordinator lock wait went p50 0s -> 75s and p90 221s -> 763s once several
# agents began running benchmark/e2e suites under with-lock, and the queue
# stopped draining. One agent's ad-hoc suite blocks that agent; the coordinator
# blocks EVERY queued branch.
lock_intent="$qdir/gate.lock.intent"
coordinator_lock="$qdir/coordinator.lock"
gate_target_file="$qdir/gate-target"
prewarm_incomplete="$qdir/prewarm-incomplete"
gate_wt="${MERGE_QUEUE_GATE_WT:-$root/.claude/worktrees/merge-gate}"
gate_cmd="${MERGE_QUEUE_GATE_CMD:-./scripts/check.sh}"
gate_cmd_is_default=1
[ -z "${MERGE_QUEUE_GATE_CMD+x}" ] || gate_cmd_is_default=0
coordinator_script="${MERGE_QUEUE_COORDINATOR_SCRIPT:-$root/scripts/merge-queue.sh}"
loaded_coordinator_hash=""
gate_timeout="${MERGE_QUEUE_GATE_TIMEOUT:-0}"
stall_timeout="${MERGE_QUEUE_STALL_TIMEOUT:-600}"
workspace_test_budget="${MERGE_QUEUE_WORKSPACE_TEST_BUDGET:-1800}"

# macOS mach_absolute_time excludes lid sleep, unlike epoch time. A suspended
# laptop cannot make test execution progress, so charge only awake seconds to
# the workspace-test budget. Python's macOS monotonic clock uses that source.
awake_seconds() {
    python3 -c 'import time; print(time.monotonic_ns() // 1000000000)'
}
monitor_interval="${MERGE_QUEUE_MONITOR_INTERVAL:-10}"
retry_interval="${MERGE_QUEUE_RETRY_INTERVAL:-5}"
poll_interval="${MERGE_QUEUE_POLL_INTERVAL:-15}"

mkdir -p "$queue_dir" "$changes_dir" "$logs"

# Patch-equivalent submissions are often consumed in long runs after an
# integration tip lands. Coalesce their worktree reclamation into one sweep.
deferred_sweep=0
active_gate_pgid=""
gate_preflight_expr=""
gate_preflight_nextest=""
gate_test_selector="legacy"
gate_dtest_status="not_attempted"
gate_dtest_elapsed_ms=""
gate_dtest_changed_files=""
gate_dtest_changed_definitions=""
gate_dtest_selected_tests=""
gate_dtest_executed_tests=""
gate_dtest_selected_not_observed_tests=""
gate_dtest_index_ms=""
gate_dtest_index_load_ms=""
gate_dtest_index_git_ms=""
gate_dtest_index_stat_ms=""
gate_dtest_index_read_ms=""
gate_dtest_index_parse_ms=""
gate_dtest_index_materialize_ms=""
gate_dtest_index_graph_ms=""
gate_dtest_index_serialize_ms=""
gate_dtest_index_map_ms=""
gate_dtest_selection_ms=""
gate_dtest_selection_git_diff_ms=""
gate_dtest_selection_old_sources_ms=""
gate_dtest_selection_old_parse_us=""
gate_dtest_selection_definition_compare_us=""
gate_dtest_selection_old_cache_hits=""
gate_dtest_selection_old_cache_misses=""
gate_dtest_selection_old_cache_lookup_us=""
gate_dtest_selection_old_cache_write_ms=""
gate_dtest_selection_graph_walk_ms=""
gate_dtest_selection_materialize_ms=""
gate_dtest_partitions=""
gate_dtest_overlay_hits=""
gate_dtest_overlay_entries=""
gate_dtest_overlay_compacted=""
gate_dtest_index_overlay_entries=""
gate_dtest_index_overlay_compacted=""
gate_dtest_target_narrowed="false"
gate_dtest_exact_filter=""
gate_dtest_filter_bytes=""
gate_dtest_required_filter_bytes=""
gate_dtest_commands=""
gate_dtest_reason=""
gate_dtest_from=""
gate_dtest_report=""
gate_nextest_scheduled_tests=""
gate_nextest_executed_tests=""
gate_nextest_passed_tests=""
gate_nextest_failed_tests=""
gate_nextest_skipped_tests=""
gate_nextest_binaries=""
# A function run_gate polls between monitor ticks; when it succeeds the gate
# is killed and reports `yielded`. Only the idle master canary sets it.
gate_yield_probe=""

# Master canary: path-scoped gates never run most of the workspace suite, so a
# test broken on master stays broken until some unrelated Cargo change happens
# to trigger a workspace gate (2026-09-25: 43 landings over ~3h). While the
# queue has no ready work, run the workspace test stage once per new master
# tip, at most once per MERGE_QUEUE_CANARY_INTERVAL seconds. Report only: a red
# canary never blocks a submission or a gate. On by default for the live
# queue, off for isolated test state unless MERGE_QUEUE_CANARY=1.
canary_enabled="${MERGE_QUEUE_CANARY:-}"
if [ -z "$canary_enabled" ]; then
    if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ]; then canary_enabled=0; else canary_enabled=1; fi
fi
canary_interval="${MERGE_QUEUE_CANARY_INTERVAL:-1800}"
canary_state="$qdir/canary.json"
canary_last_green="$qdir/canary-last-green"
canary_last_start="$qdir/canary-last-start"
if [ -n "${MERGE_QUEUE_CANARY_REPORT:-}" ]; then
    canary_report="$MERGE_QUEUE_CANARY_REPORT"
elif [ -n "${MERGE_QUEUE_STATE_DIR:-}" ]; then
    canary_report="$qdir/agents/MASTER-CANARY-RED.md"
else
    canary_report="$(witchy_agent_state_dir "$root")/MASTER-CANARY-RED.md"
fi

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
note() { printf 'merge-queue: %s\n' "$*" >&2; }
strip_ansi() { sed "s/$(printf '\033')\[[0-9;]*m//g"; }

# Host contention at a point in time. The gate's dominant cost driver is not the
# gate but the machine under it: several agents share this checkout and
# oversubscribe the CPU, so an identical branch gates in ~140s on an idle box
# and trips the 300s stall timeout on a loaded one. Journalling the 1-minute
# load average beside every attempt's phase timings is what makes those two
# separable after the fact -- without it "this branch is slow" and "the box was
# at load 80" are the same row. Best effort by construction: an unreadable
# probe yields an empty field, never a failed attempt.
load1() {
    if [ -r /proc/loadavg ]; then
        cut -d ' ' -f1 /proc/loadavg 2>/dev/null || true
    else
        sysctl -n vm.loadavg 2>/dev/null | tr -d '{}' | awk '{print $1}' || true
    fi
}
host_ncpu="$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo '')"
# Load is only interpretable against core count, so both travel together.

# The selector is state, not a caller-controlled path. Only these two sibling
# Cargo generations are valid; old state directories have no selector and use
# the historical target/ generation.
gate_target_generation() {
    local selected="target"
    if [ -f "$gate_target_file" ]; then
        selected="$(cat "$gate_target_file" 2>/dev/null || true)"
    fi
    case "$selected" in
        target | target-prewarm) ;;
        *)
            note "WARNING: invalid gate-target '$selected'; using target"
            selected="target"
            ;;
    esac
    printf '%s\n' "$selected"
}

inactive_gate_target_generation() { # inactive_gate_target_generation <active>
    case "$1" in
        target) printf 'target-prewarm\n' ;;
        target-prewarm) printf 'target\n' ;;
        *) return 1 ;;
    esac
}

promote_gate_target() { # promote_gate_target <generation>
    local generation="$1" tmp
    case "$generation" in target | target-prewarm) ;; *) return 1 ;; esac
    tmp="$(mktemp "$qdir/.gate-target-XXXXXX")" || return 1
    if ! printf '%s\n' "$generation" >"$tmp" || ! mv -f "$tmp" "$gate_target_file"; then
        rm -f "$tmp"
        return 1
    fi
}

# `git merge-tree --write-tree` reports conflicts through its exit status, but
# normally writes the synthetic result into the repository object DB. Submit is
# otherwise metadata-only and must work when `.git` is readable but not
# writable, so keep the result in a temporary object DB and read repository
# objects through an alternate.
branch_merges_cleanly() { # branch_merges_cleanly <branch>
    local branch="$1" object_dir alternates rc=0
    object_dir="$(mktemp -d "${TMPDIR:-/tmp}/witchy-merge-tree-XXXXXX")"
    alternates="$(git -C "$root" rev-parse --absolute-git-dir)/objects"
    if [ -n "${GIT_ALTERNATE_OBJECT_DIRECTORIES:-}" ]; then
        alternates="$alternates:$GIT_ALTERNATE_OBJECT_DIRECTORIES"
    fi
    GIT_OBJECT_DIRECTORY="$object_dir" GIT_ALTERNATE_OBJECT_DIRECTORIES="$alternates" \
        git -C "$root" merge-tree --write-tree --name-only master \
            "refs/heads/$branch" >/dev/null 2>&1 || rc=$?
    rm -rf "$object_dir"
    return "$rc"
}

# Managed sandboxes can deny signalling a live process with EPERM. Every
# coordinator/lock decision must distinguish that from a missing PID; otherwise
# agents spawn duplicate coordinators and steal live gate locks.
pid_is_alive() {
    local pid="${1:-}" error
    case "$pid" in
        '' | *[!0-9]*) return 1 ;;
    esac
    if error="$(kill -0 "$pid" 2>&1)"; then
        return 0
    fi
    case "$error" in
        *"Operation not permitted"* | *"operation not permitted"* | *"not permitted"*)
            return 0
            ;;
    esac
    return 1
}

case "$stall_timeout" in
    '' | *[!0-9]* | 0)
        note "MERGE_QUEUE_STALL_TIMEOUT must be a positive integer"
        exit 2
        ;;
esac
case "$gate_timeout" in
    '' | *[!0-9]*)
        note "MERGE_QUEUE_GATE_TIMEOUT must be a non-negative integer (0 disables it)"
        exit 2
        ;;
esac
case "$canary_enabled:$canary_interval" in
    [01]:[0-9]*) case "$canary_interval" in *[!0-9]*) canary_enabled=bad ;; esac ;;
    *) canary_enabled=bad ;;
esac
if [ "$canary_enabled" = bad ]; then
    note "MERGE_QUEUE_CANARY must be 0 or 1 and MERGE_QUEUE_CANARY_INTERVAL a non-negative integer"
    exit 2
fi
case "$workspace_test_budget" in
    '' | *[!0-9]*)
        note "MERGE_QUEUE_WORKSPACE_TEST_BUDGET must be a non-negative integer (0 disables it)"
        exit 2
        ;;
esac
gate_timeout_display() {
    if [ "$gate_timeout" -eq 0 ]; then
        printf 'disabled'
    else
        printf '%ss' "$gate_timeout"
    fi
}

# The last `==> [N] stage (t+Ns)` marker in a gate log = the stage running now.
current_stage() { { strip_ansi <"$1" 2>/dev/null || true; } | { grep -E '^==> \[' || true; } | tail -1 | sed 's/^==> //'; }
# All markers, one line: "[1] build (t+0s);[2] clippy (t+41s);..."
stage_summary() { { strip_ansi <"$1" 2>/dev/null || true; } | { grep -E '^==> \[' || true; } | sed 's/^==> //' | paste -sd';' -; }
# Extract a one-line failure summary from a red gate log: the failing stage +
# first error/FAIL line. Helps agents diagnose without reading the full log.
failure_summary() {
    local log="$1" plain
    plain="$(strip_ansi <"$log" 2>/dev/null)" || return 0
    local stage; stage="$(echo "$plain" | grep -E '^==> \[' | tail -1 | sed 's/^==> //')"
    local fail_line=""
    fail_line="$(echo "$plain" | grep -m1 -E '^ *(TRY [0-9]+ )?(FAIL|TIMEOUT) \[' | sed 's/^ *//' || true)"
    [ -z "$fail_line" ] && fail_line="$(echo "$plain" | grep -m1 '^error\[E\|^error:' | head -c120 || true)"
    [ -z "$fail_line" ] && fail_line="$(echo "$plain" | grep -m1 'could not compile' || true)"
    local summary="${stage:+$stage}"
    [ -n "$fail_line" ] && summary="${summary:+$summary: }$fail_line"
    echo "${summary:-unknown failure}"
}

# Tests that failed and then passed on retry. nextest prints these as
# `FLAKY [ ... ] (n/m) binary test`, and on a green gate they are otherwise
# invisible: the retry absorbs them and nobody learns the test is rotting.
# Journaling the count is what turns "aggressively fix or delete flaky tests"
# into a decision backed by numbers rather than by whoever got bitten last.
flaky_summary() { # flaky_summary <log>
    local log="$1" plain
    plain="$(strip_ansi <"$log" 2>/dev/null)" || return 0
    echo "$plain" | grep -cE '^ *FLAKY \[' || true
}

flaky_tests() { # flaky_tests <log> -- the names, for triage
    local log="$1" plain
    plain="$(strip_ansi <"$log" 2>/dev/null)" || return 0
    echo "$plain" | grep -E '^ *FLAKY \[' | sed 's/^ *//' | cut -c1-300 | tr '\n' ';'
}

# The tests that failed or were killed in a gate log, as a nextest filter
# expression. Bounded to 8 so a broadly-broken branch cannot build a filter
# longer than the suite it replaces.
failing_test_expr() { # failing_test_expr <log>
    local log="$1" names
    names="$(strip_ansi <"$log" 2>/dev/null \
        | grep -aE '^\s*(TRY [0-9]+ )?(FAIL|TIMEOUT|TMT) \[' \
        | sed -E 's/.*\) +[^ ]+ +([A-Za-z0-9_:]+).*/\1/' \
        | grep -aE '^[A-Za-z0-9_:]+$' | sort -u | head -8)"
    [ -n "$names" ] || return 0
    local expr="" n
    while IFS= read -r n; do
        [ -n "$n" ] || continue
        # Nextest names tests as `binary::module::test`, while cargo's
        # single-binary summary reports just `module::test`.  Replay by
        # suffix so a filter learned from either format selects the same test
        # instead of silently discovering zero tests.
        expr="${expr:+$expr or }test(/^(?:.*::)?${n}$/)"
    done <<EOF
$names
EOF
    printf '%s' "$expr"
}

# The Cargo package/target shared by every failed nextest test. A single
# validated target lets the next attempt compile only the binary needed for
# its failure-first replay. Mixed or unparseable targets deliberately return
# nothing so check.sh keeps the existing landing selection.
failing_test_nextest() { # failing_test_nextest <log>
    local log="$1" targets target package binary
    targets="$(strip_ansi <"$log" 2>/dev/null \
        | grep -aE '^[[:space:]]*(TRY [0-9]+ )?(FAIL|TIMEOUT|TMT) \[' \
        | sed -E 's/^[[:space:]]*(TRY [0-9]+ )?[A-Z]+ \[[^]]*\][[:space:]]*(\([[:space:]]*[0-9]+[[:space:]]*\/[[:space:]]*[0-9]+[[:space:]]*\)[[:space:]]*)?//' \
        | awk '{print $1}' | grep -aE '^[A-Za-z0-9_-]+(::[A-Za-z0-9_\/-]+)?$' \
        | sort -u | head -2)"
    [ -n "$targets" ] || return 0
    [ "$(printf '%s\n' "$targets" | wc -l | tr -d ' ')" = 1 ] || return 0
    target="$targets"
    case "$target" in
        *::*)
            package="${target%%::*}"
            binary="${target#*::}"
            case "$binary" in
                bin/*) printf '%s\n' "-p $package --bin ${binary#bin/}" ;;
                */*) return 0 ;;
                *) printf '%s\n' "-p $package --test $binary" ;;
            esac
            ;;
        *) printf '%s\n' "-p $target --lib" ;;
    esac
}

# Remember them on the change so the NEXT attempt replays them first.
remember_preflight() { # remember_preflight <change-id> <expr> [nextest-selector]
    local id="$1" expr="$2" nextest="${3:-}" cf tmp
    [ -n "$id" ] && [ -n "$expr" ] || return 0
    acquire_change_lock
    while IFS= read -r cf; do
        jq -e --arg id "$id" 'select(.change_id==$id)' "$cf" >/dev/null 2>&1 || continue
        tmp="$cf.tmp.$$"
        jq --arg e "$expr" --arg n "$nextest" \
            '.preflight=$e | if $n == "" then del(.preflight_nextest) else .preflight_nextest=$n end' \
            "$cf" >"$tmp" && mv "$tmp" "$cf"
        break
    done < <(change_files_for_id "$id")
    release_change_lock
}

preflight_nextest_for() { # preflight_nextest_for <change-id>
    local id="$1" cf
    [ -n "$id" ] || return 0
    while IFS= read -r cf; do
        jq -er --arg id "$id" 'select(.change_id==$id) | .preflight_nextest // empty' \
            "$cf" 2>/dev/null && return 0
    done < <(change_files_for_id "$id")
    return 0
}

preflight_expr_for() { # preflight_expr_for <change-id>
    local id="$1" cf
    [ -n "$id" ] || return 0
    while IFS= read -r cf; do
        jq -er --arg id "$id" 'select(.change_id==$id) | .preflight // empty' "$cf" 2>/dev/null && return 0
    done < <(change_files_for_id "$id")
    return 0
}

record() { # record <event> <branch> [key value]...
    local event="$1" branch="$2"; shift 2
    local args=(--arg ts "$(now)" --arg event "$event" --arg branch "$branch")
    local extra=""
    while [ "$#" -ge 2 ]; do
        args+=(--arg "$1" "$2")
        extra="$extra, ${1}: \$${1}"
        shift 2
    done
    jq -cn "${args[@]}" "{ts: \$ts, event: \$event, branch: \$branch${extra}}" >>"$journal"
}

# Low-disk guard. Agents' per-worktree build caches filled this volume to
# 13 GB and then 8 GB free on 2026-09-24 while every candidate cache sat idle
# for hours, and a full disk corrupts the gate's own prewarm. Below
# MERGE_QUEUE_DISK_LOW_GB free, reclaim idle build caches in the background,
# oldest first, until MERGE_QUEUE_DISK_TARGET_GB is free. The reclaim script
# skips the merge-gate worktree, any cache with an open handle, and anything
# touched within six hours; deleting a cache costs a rebuild, never source.
disk_guard() {
    [ -z "${MERGE_QUEUE_TEST_ROOT:-}" ] || return 0
    local low="${MERGE_QUEUE_DISK_LOW_GB:-40}" target="${MERGE_QUEUE_DISK_TARGET_GB:-60}"
    [ "$low" -gt 0 ] 2>/dev/null || return 0
    local reclaim="$root/scripts/reclaim-build-caches.sh"
    [ -x "$reclaim" ] || return 0
    local free_mb; free_mb="$(df -m "$root" 2>/dev/null | awk 'NR==2 {print $4}')"
    case "$free_mb" in '' | *[!0-9]*) return 0 ;; esac
    [ "$free_mb" -lt $((low * 1024)) ] || return 0
    local lock="$qdir/disk-guard.lock"
    if ! mkdir "$lock" 2>/dev/null; then
        local holder; holder="$(cat "$lock/pid" 2>/dev/null || true)"
        case "$holder" in
            '' | *[!0-9]*) ;;
            *) kill -0 "$holder" 2>/dev/null && return 0 ;;
        esac
        rm -rf "$lock"; mkdir "$lock" 2>/dev/null || return 0
    fi
    record disk_low "" free_mb "$free_mb" low_gb "$low" target_gb "$target"
    note "disk low (${free_mb} MB free); reclaiming idle build caches in the background"
    nohup sh -c '"$1" --include-unlanded --include-clones --min-age-hours 6 \
        --keep-free-gb "$2" --apply; rm -rf "$3"' sh "$reclaim" "$target" "$lock" \
        >"$qdir/disk-guard.log" 2>&1 </dev/null &
    printf '%s\n' "$!" >"$lock/pid"
}

# Extract the primary workspace nextest invocation once, immediately after the
# gate. Auxiliary queue/fuzz shards have their own stage markers and must not
# overwrite the product counts. A red run can report `15/1714 tests run`; keep
# both the scheduled and actually executed values.
capture_gate_test_metrics() { # capture_gate_test_metrics <gate-log>
    local values=""
    values="$(LC_ALL=C awk '
        BEGIN { scheduled=executed=passed=failed=skipped=binaries=""; saw_stage=workspace=0 }
        {
            line=$0
            gsub(/\033\[[0-9;]*m/, "", line)
            if (line ~ /^==> \[[0-9]+\] /) {
                saw_stage=1
                workspace=(line ~ /^==> \[1\] tests \(workspace\)/)
            }
            if ((!saw_stage || workspace) && line ~ /Starting [0-9]+ tests? across [0-9]+ binar(y|ies)/) {
                value=line
                sub(/^.*Starting /, "", value)
                sub(/^[[:space:]]*/, "", value)
                split(value, fields, " ")
                scheduled=fields[1]
                binaries=fields[4]
            }
            if ((!saw_stage || workspace) && line ~ /Summary \[ *[0-9.]+s\]/) {
                value=line
                sub(/^.*s\] /, "", value)
                n=split(value, fields, " ")
                run=fields[1]
                sub(/\/.*/, "", run)
                if (run ~ /^[0-9]+$/) executed=run
                for (i=2; i<=n; i++) {
                    label=fields[i]
                    gsub(/[:,]/, "", label)
                    number=fields[i-1]
                    gsub(/[^0-9]/, "", number)
                    if (number == "") continue
                    if (label == "passed") passed=number
                    else if (label == "failed") failed=number
                    else if (label == "skipped") skipped=number
                }
            }
        }
        END { printf "%s\034%s\034%s\034%s\034%s\034%s", scheduled, executed, passed, failed, skipped, binaries }
    ' "$1" 2>/dev/null || true)"
    IFS=$'\x1c' read -r gate_nextest_scheduled_tests gate_nextest_executed_tests \
        gate_nextest_passed_tests gate_nextest_failed_tests \
        gate_nextest_skipped_tests gate_nextest_binaries <<<"$values"
}

# Record one coordinator attempt with phase boundaries captured by process_one.
# Keep elapsed_s as the actual gate duration for existing journal consumers;
# attempt_elapsed_s covers dequeue through the final validation decision. All
# values are integer wall seconds because macOS /bin/date has no portable
# sub-second epoch format.
record_attempt() { # event branch start prepared locked gate-start gate-end finish [key value]...
    local event="$1" branch="$2" attempt_start="$3" prepare_finished="$4"
    local lock_acquired="$5" gate_start="$6" gate_end="$7" finish="$8"
    shift 8
    # One choke point for every outcome (merged/red/timeout/requeued/...), so
    # the contention covariate can never be present for some events and absent
    # for others -- which would silently bias any comparison drawn from it.
    local contention=()
    [ -n "$gate_load_start" ] && contention+=(gate_load_start "$gate_load_start")
    [ -n "$gate_load_end" ] && contention+=(gate_load_end "$gate_load_end")
    [ -n "$host_ncpu" ] && contention+=(host_ncpu "$host_ncpu")
    local selection=(test_selector "${gate_test_selector:-legacy}"
        dtest_status "${gate_dtest_status:-not_attempted}")
    [ -n "${gate_dtest_elapsed_ms:-}" ] && selection+=(dtest_elapsed_ms "$gate_dtest_elapsed_ms")
    [ -n "${gate_dtest_changed_files:-}" ] && selection+=(dtest_changed_files "$gate_dtest_changed_files")
    [ -n "${gate_dtest_changed_definitions:-}" ] && selection+=(dtest_changed_definitions "$gate_dtest_changed_definitions")
    [ -n "${gate_dtest_selected_tests:-}" ] && selection+=(dtest_selected_tests "$gate_dtest_selected_tests")
    [ -n "${gate_dtest_executed_tests:-}" ] && selection+=(dtest_executed_tests "$gate_dtest_executed_tests")
    [ -n "${gate_dtest_selected_not_observed_tests:-}" ] && selection+=(dtest_selected_not_observed_tests "$gate_dtest_selected_not_observed_tests")
    [ -n "${gate_dtest_index_ms:-}" ] && selection+=(dtest_index_ms "$gate_dtest_index_ms")
    [ -n "${gate_dtest_index_load_ms:-}" ] && selection+=(dtest_index_load_ms "$gate_dtest_index_load_ms")
    [ -n "${gate_dtest_index_git_ms:-}" ] && selection+=(dtest_index_git_ms "$gate_dtest_index_git_ms")
    [ -n "${gate_dtest_index_stat_ms:-}" ] && selection+=(dtest_index_stat_ms "$gate_dtest_index_stat_ms")
    [ -n "${gate_dtest_index_read_ms:-}" ] && selection+=(dtest_index_read_ms "$gate_dtest_index_read_ms")
    [ -n "${gate_dtest_index_parse_ms:-}" ] && selection+=(dtest_index_parse_ms "$gate_dtest_index_parse_ms")
    [ -n "${gate_dtest_index_materialize_ms:-}" ] && selection+=(dtest_index_materialize_ms "$gate_dtest_index_materialize_ms")
    [ -n "${gate_dtest_index_graph_ms:-}" ] && selection+=(dtest_index_graph_ms "$gate_dtest_index_graph_ms")
    [ -n "${gate_dtest_index_serialize_ms:-}" ] && selection+=(dtest_index_serialize_ms "$gate_dtest_index_serialize_ms")
    [ -n "${gate_dtest_index_map_ms:-}" ] && selection+=(dtest_index_map_ms "$gate_dtest_index_map_ms")
    [ -n "${gate_dtest_selection_ms:-}" ] && selection+=(dtest_selection_ms "$gate_dtest_selection_ms")
    [ -n "${gate_dtest_selection_git_diff_ms:-}" ] && selection+=(dtest_selection_git_diff_ms "$gate_dtest_selection_git_diff_ms")
    [ -n "${gate_dtest_selection_old_sources_ms:-}" ] && selection+=(dtest_selection_old_sources_ms "$gate_dtest_selection_old_sources_ms")
    [ -n "${gate_dtest_selection_old_parse_us:-}" ] && selection+=(dtest_selection_old_parse_us "$gate_dtest_selection_old_parse_us")
    [ -n "${gate_dtest_selection_definition_compare_us:-}" ] && selection+=(dtest_selection_definition_compare_us "$gate_dtest_selection_definition_compare_us")
    [ -n "${gate_dtest_selection_old_cache_hits:-}" ] && selection+=(dtest_selection_old_cache_hits "$gate_dtest_selection_old_cache_hits")
    [ -n "${gate_dtest_selection_old_cache_misses:-}" ] && selection+=(dtest_selection_old_cache_misses "$gate_dtest_selection_old_cache_misses")
    [ -n "${gate_dtest_selection_old_cache_lookup_us:-}" ] && selection+=(dtest_selection_old_cache_lookup_us "$gate_dtest_selection_old_cache_lookup_us")
    [ -n "${gate_dtest_selection_old_cache_write_ms:-}" ] && selection+=(dtest_selection_old_cache_write_ms "$gate_dtest_selection_old_cache_write_ms")
    [ -n "${gate_dtest_selection_graph_walk_ms:-}" ] && selection+=(dtest_selection_graph_walk_ms "$gate_dtest_selection_graph_walk_ms")
    [ -n "${gate_dtest_selection_materialize_ms:-}" ] && selection+=(dtest_selection_materialize_ms "$gate_dtest_selection_materialize_ms")
    [ -n "${gate_dtest_partitions:-}" ] && selection+=(dtest_partitions "$gate_dtest_partitions")
    [ -n "${gate_dtest_overlay_hits:-}" ] && selection+=(dtest_overlay_hits "$gate_dtest_overlay_hits")
    [ -n "${gate_dtest_overlay_entries:-}" ] && selection+=(dtest_overlay_entries "$gate_dtest_overlay_entries")
    [ -n "${gate_dtest_overlay_compacted:-}" ] && selection+=(dtest_overlay_compacted "$gate_dtest_overlay_compacted")
    [ -n "${gate_dtest_index_overlay_entries:-}" ] && selection+=(dtest_index_overlay_entries "$gate_dtest_index_overlay_entries")
    [ -n "${gate_dtest_index_overlay_compacted:-}" ] && selection+=(dtest_index_overlay_compacted "$gate_dtest_index_overlay_compacted")
    selection+=(dtest_target_narrowed "${gate_dtest_target_narrowed:-false}")
    [ -n "${gate_dtest_exact_filter:-}" ] && selection+=(dtest_exact_filter "$gate_dtest_exact_filter")
    [ -n "${gate_dtest_filter_bytes:-}" ] && selection+=(dtest_filter_bytes "$gate_dtest_filter_bytes")
    [ -n "${gate_dtest_required_filter_bytes:-}" ] && selection+=(dtest_required_filter_bytes "$gate_dtest_required_filter_bytes")
    [ -n "${gate_dtest_commands:-}" ] && selection+=(dtest_commands "$gate_dtest_commands")
    [ -n "${gate_dtest_reason:-}" ] && selection+=(dtest_reason "$gate_dtest_reason")
    [ -n "${gate_nextest_scheduled_tests:-}" ] && selection+=(nextest_scheduled_tests "$gate_nextest_scheduled_tests")
    [ -n "${gate_nextest_executed_tests:-}" ] && selection+=(nextest_executed_tests "$gate_nextest_executed_tests")
    [ -n "${gate_nextest_passed_tests:-}" ] && selection+=(nextest_passed_tests "$gate_nextest_passed_tests")
    [ -n "${gate_nextest_failed_tests:-}" ] && selection+=(nextest_failed_tests "$gate_nextest_failed_tests")
    [ -n "${gate_nextest_skipped_tests:-}" ] && selection+=(nextest_skipped_tests "$gate_nextest_skipped_tests")
    [ -n "${gate_nextest_binaries:-}" ] && selection+=(nextest_binaries "$gate_nextest_binaries")

    # RFC-0159 E. `reason` already carries the failing stage and failure line,
    # concatenated into prose -- so grouping reds by cause meant re-parsing a
    # human string. Split it here, at the same choke point, so grouping is a
    # field lookup. `reason` is KEPT: it is the readable form and existing
    # consumers read it.
    local classified=()
    case "$event" in
        red | batch_red | timeout)
            local kv=("$@") reason_value="" j=0
            while [ "$j" -lt "${#kv[@]}" ]; do
                if [ "${kv[$j]}" = "reason" ]; then
                    reason_value="${kv[$((j + 1))]}"
                    break
                fi
                j=$((j + 2))
            done
            local failing_stage="" failure_line="" red_class="unknown"
            if [ -n "$reason_value" ]; then
                failing_stage="$(printf '%s' "$reason_value" \
                    | sed -n 's/^\[[0-9]*\] \(.*\) (t+.*/\1/p')"
                failure_line="${reason_value#*: }"
                [ "$failure_line" = "$reason_value" ] && failure_line=""
            fi
            # Deliberately coarse. The one judgement it makes is the load one:
            # a stall timeout on an oversubscribed box is weather, the same
            # timeout on an idle box is a flake worth a bug -- and since the
            # contention covariate landed, that distinction is available here
            # rather than guessed later.
            local loaded=0
            if [ -n "$gate_load_start" ] && [ -n "$host_ncpu" ] && [ "$host_ncpu" -gt 0 ] 2>/dev/null; then
                loaded="$(awk -v l="$gate_load_start" -v n="$host_ncpu" \
                    'BEGIN { print (l / n >= 2.0) ? 1 : 0 }' 2>/dev/null || echo 0)"
            fi
            case "$reason_value" in
                *"no log output"* | *"exceeded"* | *"timed out"* | *"creating test list failed"*)
                    if [ "$loaded" = "1" ]; then red_class="infra"; else red_class="timeout"; fi ;;
                *)
                    case "$failure_line" in
                        "error: linking with "*" failed"*) red_class="compile" ;;
                        *)
                            case "$failing_stage" in
                                tests*)                 red_class="test" ;;
                                preflight*)
                                    # A replayed nextest failure carries the
                                    # failing test in its TRY/FAIL line, while
                                    # the stage name is preflight. Attribute
                                    # only those explicit test failures here;
                                    # missing output and other preflight
                                    # failures remain unknown/infra below.
                                    case "$failure_line" in
                                        *TRY*FAIL* | *TIMEOUT*) red_class="test" ;;
                                    esac ;;
                                *compile*)              red_class="compile" ;;
                                *clippy*)               red_class="clippy" ;;
                                *fmt*)                  red_class="fmt" ;;
                                *"queue infrastructure"*) red_class="infra" ;;
                                *)  [ "$event" = "timeout" ] && red_class="timeout" ;;
                            esac ;;
                    esac ;;
            esac
            classified+=(failing_stage "$failing_stage" failure_line "$failure_line" red_class "$red_class")
            ;;
    esac

    record "$event" "$branch" \
        attempt_timing_schema "1" \
        elapsed_s "$((gate_end - gate_start))" \
        attempt_elapsed_s "$((finish - attempt_start))" \
        prepare_elapsed_s "$((prepare_finished - attempt_start))" \
        lock_wait_s "$((lock_acquired - prepare_finished))" \
        preflight_elapsed_s "$((gate_start - lock_acquired))" \
        gate_elapsed_s "$((gate_end - gate_start))" \
        landing_elapsed_s "$((finish - gate_end))" \
        ${contention[@]+"${contention[@]}"} \
        ${selection[@]+"${selection[@]}"} \
        ${classified[@]+"${classified[@]}"} \
        "$@"
}

branch_key() { printf '%s\n' "$1" | tr '/' '~'; }

# Urgent submissions sort ahead of both ordinary epoch-prefixed entries and
# legacy 0front entries. Inverting the epoch also makes the newest explicit
# reprioritization win instead of accumulating behind every older --front.
front_stamp() {
    local epoch
    epoch="$(date +%s)"
    printf '00front-%010d-%d' "$((9999999999 - epoch))" "$$"
}
change_file_for_branch() { printf '%s/%s.json\n' "$changes_dir" "$(branch_key "$1")"; }

# Change IDs are random hashes, but records are named for branches or archived
# history. A registry lookup used to fork jq once per record; at 2,106 records
# that put 19-29 seconds of metadata scanning between the gate lock and the
# first test. Let one grep process narrow the candidates, then retain jq as the
# authoritative exact-field check at every caller. False textual matches are
# harmless, and missing or corrupt records keep each caller's existing result.
change_files_for_id() { # change_files_for_id <change-id>
    [ -n "$1" ] || return 0
    find "$changes_dir" -type f -name '*.json' \
        -exec grep -lF -- "$1" {} + 2>/dev/null || true
}

new_change_id() { # new_change_id <branch>
    printf '%s\0%s\0%s\0%s\n' "$1" "$(date +%s)" "$$" "${RANDOM:-0}" \
        | git -C "$root" hash-object --stdin \
        | sed 's/^/mq-/'
}
new_attempt_id() { # new_attempt_id <branch>
    new_change_id "$1-attempt" | sed 's/^mq-/mqa-/'
}

change_id_for_branch() { # change_id_for_branch <branch>
    local cf; cf="$(change_file_for_branch "$1")"
    [ -f "$cf" ] || return 1
    jq -er --arg branch "$1" 'select(.branch==$branch) | .change_id' "$cf"
}

change_state_for_id() { # change_state_for_id <change-id>
    local cf
    while IFS= read -r cf; do
        jq -er --arg id "$1" 'select(.change_id==$id) | .state' "$cf" 2>/dev/null && return 0
    done < <(change_files_for_id "$1")
    return 1
}

change_branch_for_id() { # change_branch_for_id <change-id>
    local cf
    while IFS= read -r cf; do
        jq -er --arg id "$1" 'select(.change_id==$id) | .branch' "$cf" 2>/dev/null && return 0
    done < <(change_files_for_id "$1")
    return 1
}

set_change_state() { # set_change_state <change-id> <state> [current-sha] [current-attempt]
    local id="$1" state="$2" expected_sha="${3:-}" expected_attempt="${4:-}" cf tmp
    [ -n "$id" ] || return 0
    acquire_change_lock
    while IFS= read -r cf; do
        jq -e --arg id "$id" 'select(.change_id==$id)' "$cf" >/dev/null 2>&1 || continue
        if [ -n "$expected_sha" ] \
            && [ "$(jq -r '.current_sha // empty' "$cf")" != "$expected_sha" ]; then
            release_change_lock
            return 1
        fi
        if [ -n "$expected_attempt" ] \
            && [ "$(jq -r '.current_attempt // empty' "$cf")" != "$expected_attempt" ]; then
            release_change_lock
            return 1
        fi
        tmp="$cf.tmp.$$"
        jq --arg state "$state" --arg updated "$(now)" \
            '.state=$state | .updated=$updated' "$cf" >"$tmp"
        mv "$tmp" "$cf"
        release_change_lock
        return 0
    done < <(change_files_for_id "$id")
    release_change_lock
    return 1
}

archive_change_record() { # archive_change_record <branch>
    local cf id
    cf="$(change_file_for_branch "$1")"
    [ -f "$cf" ] || return 0
    id="$(jq -r '.change_id' "$cf")"
    mv "$cf" "$changes_dir/history-$id.json"
}

queue_entry_matches() { # queue_entry_matches <queue-file> <change-id> <sha> <attempt-id>
    [ -f "$1" ] || return 1
    jq -e --arg id "$2" --arg sha "$3" --arg attempt "$4" \
        'select((.change_id // "")==$id and (.sha // "")==$sha and
          (.attempt_id // "")==$attempt)' "$1" >/dev/null 2>&1
}

claim_queue_entry_for_gate() { # claim_queue_entry_for_gate <queue-file> <change-id> <sha> <attempt-id>
    local qf="$1" id="$2" sha="$3" attempt="$4" cf tmp
    acquire_change_lock
    if ! queue_entry_matches "$qf" "$id" "$sha" "$attempt"; then
        release_change_lock
        return 1
    fi
    while IFS= read -r cf; do
        jq -e --arg id "$id" --arg sha "$sha" --arg attempt "$attempt" \
            'select(.change_id==$id and (.current_sha // "")==$sha and
              (.current_attempt // "")==$attempt and .state=="queued")' \
            "$cf" >/dev/null 2>&1 || continue
        tmp="$cf.tmp.$$"
        jq --arg state gating --arg updated "$(now)" \
            '.state=$state | .updated=$updated' "$cf" >"$tmp"
        mv "$tmp" "$cf"
        release_change_lock
        return 0
    done < <(change_files_for_id "$id")
    release_change_lock
    return 1
}

# A coordinator can die after atomically claiming an attempt but before it
# returns that attempt to queued or records a terminal state. The queue file is
# deliberately retained during gating, so a new singleton owner can prove the
# claim is orphaned and make it eligible again. This runs only after acquiring
# coordinator.lock; no live coordinator can be preparing or gating an attempt
# concurrently.
recover_orphaned_change_claims() {
    local cf state id sha attempt branch qf tmp
    acquire_change_lock
    # Only an entry that is still present in the live queue can be recovered.
    # Walking the complete historical change ledger here spawned one `jq` per
    # old record on every coordinator restart, delaying a recovered gate by
    # minutes. Start with the (normally tiny) live queue, then prove that its
    # current change record is the matching in-flight attempt.
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        branch="$(jq -r '.branch // empty' "$qf")"
        id="$(jq -r '.change_id // empty' "$qf")"
        sha="$(jq -r '.sha // empty' "$qf")"
        attempt="$(jq -r '.attempt_id // empty' "$qf")"
        [ -n "$branch" ] && [ -n "$id" ] && [ -n "$sha" ] && [ -n "$attempt" ] || continue
        cf="$(change_file_for_branch "$branch")"
        [ -f "$cf" ] || continue
        state="$(jq -r '.state // empty' "$cf")"
        case "$state" in gating | validated) ;; *) continue ;; esac
        queue_entry_matches "$qf" "$id" "$sha" "$attempt" || continue
        jq -e --arg id "$id" --arg sha "$sha" --arg attempt "$attempt" \
            'select(.change_id==$id and (.current_sha // "")==$sha and
              (.current_attempt // "")==$attempt)' "$cf" >/dev/null 2>&1 || continue
        tmp="$cf.tmp.$$"
        jq --arg state queued --arg updated "$(now)" \
            '.state=$state | .updated=$updated' "$cf" >"$tmp"
        mv "$tmp" "$cf"
        record recovered "$branch" change_id "$id" attempt_id "$attempt" \
            submitted_sha "$sha" reason "orphaned coordinator claim"
        note "recovered orphaned $state claim for $branch"
    done
    release_change_lock
}

consume_queue_entry() { # consume_queue_entry <queue-file> <change-id> <sha> <attempt-id>
    local qf="$1" id="$2" sha="$3" attempt="$4"
    acquire_change_lock
    if ! queue_entry_matches "$qf" "$id" "$sha" "$attempt"; then
        release_change_lock
        return 1
    fi
    rm -f "$qf" "$qf.nobatch" "$qf.batch-limit"
    release_change_lock
}

# `--front` expedites ONE urgent change; it must not become a standing claim on
# the batch-leader slot. A batch red deliberately RETAINS every member's queue
# file (a red batch indicts no single member), so without this a front-stamped
# member re-leads the next cycle, and the one after that, and every ready
# candidate behind it starves. Observed 2026-08-27: 3h41m with zero merges, 21
# queued, 14 of them ready, while one front-pinned change gated four times and
# redded four times (BUG-633).
#
# Demotion is not a punishment and not an eviction: the change stays queued at
# an ordinary FIFO stamp, and `submit --front` can deliberately re-prioritize it
# when someone decides it really is the urgent one again.
demote_front_entry() { # demote_front_entry <queue-file>
    local qf="$1" base branch new sidecar
    [ -f "$qf" ] || return 0
    base="$(basename "$qf")"
    case "$base" in 00front-* | 0front-*) ;; *) return 0 ;; esac
    branch="$(jq -r '.branch // empty' "$qf" 2>/dev/null || true)"
    [ -n "$branch" ] || return 0
    new="$queue_dir/$(date +%s)-$(branch_key "$branch").json"
    [ "$new" = "$qf" ] && return 0
    mv "$qf" "$new" || return 1
    for sidecar in nobatch batch-limit; do
        if [ -f "$qf.$sidecar" ]; then mv "$qf.$sidecar" "$new.$sidecar" || true; fi
    done
    note "  demoted $branch from the queue head after a red (--front does not survive a failure)"
    record demoted "$branch" reason "red while holding the queue head" entry "$(basename "$new")"
    return 0
}

mark_queue_entry() { # mark_queue_entry <queue-file> <change-id> <sha> <attempt-id> <suffix>
    local qf="$1" id="$2" sha="$3" attempt="$4" suffix="$5"
    acquire_change_lock
    if ! queue_entry_matches "$qf" "$id" "$sha" "$attempt"; then
        release_change_lock
        return 1
    fi
    touch "$qf.$suffix"
    release_change_lock
}

set_queue_batch_limit() { # set_queue_batch_limit <queue-file> <change-id> <sha> <attempt-id> <limit>
    local qf="$1" id="$2" sha="$3" attempt="$4" limit="$5"
    acquire_change_lock
    if ! queue_entry_matches "$qf" "$id" "$sha" "$attempt"; then
        release_change_lock
        return 1
    fi
    echo "$limit" >"$qf.batch-limit"
    release_change_lock
}

write_change_record() { # write_change_record <branch> <id> <sha> <after-json> <state> <attempt-id>
    local branch="$1" id="$2" sha="$3" after="$4" state="$5" attempt="$6" cf tmp created preflight preflight_nextest
    cf="$(change_file_for_branch "$branch")"
    created="$(jq -r '.created // empty' "$cf" 2>/dev/null || true)"
    # A red records its exact failing tests on the logical change. Preserve
    # that replay when submit creates the next attempt; replacing the record
    # here used to discard it before the coordinator could run the preflight.
    preflight="$(jq -r '.preflight // empty' "$cf" 2>/dev/null || true)"
    preflight_nextest="$(jq -r '.preflight_nextest // empty' "$cf" 2>/dev/null || true)"
    [ -n "$created" ] || created="$(now)"
    tmp="$cf.tmp.$$"
    jq -cn --arg branch "$branch" --arg id "$id" --arg sha "$sha" \
        --arg created "$created" --arg updated "$(now)" --arg state "$state" --arg attempt "$attempt" \
        --arg preflight "$preflight" --arg preflight_nextest "$preflight_nextest" --argjson after "$after" \
        '{schema:1, change_id:$id, branch:$branch, current_sha:$sha,
          current_attempt:$attempt, after:$after, state:$state,
          created:$created, updated:$updated}
          | if $preflight == "" then . else .preflight=$preflight end
          | if $preflight_nextest == "" then . else .preflight_nextest=$preflight_nextest end' >"$tmp"
    mv "$tmp" "$cf"
}

retrofit_queued_change() { # retrofit_queued_change <branch> <change-id> <attempt-id>
    local branch="$1" id="$2" attempt="$3" qf tmp
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        [ "$(jq -r '.branch // empty' "$qf")" = "$branch" ] || continue
        tmp="$qf.tmp.$$"
        jq --arg id "$id" --arg attempt "$attempt" \
            '.schema=2 | .change_id=$id | .attempt_id=$attempt | .after=(.after // [])' "$qf" >"$tmp"
        mv "$tmp" "$qf"
    done
}

known_parent_change_id() { # known_parent_change_id <branch>
    local branch="$1" id attempt qf last state sha
    if id="$(change_id_for_branch "$branch" 2>/dev/null)"; then
        printf '%s\n' "$id"
        return 0
    fi
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        [ "$(jq -r '.branch // empty' "$qf")" = "$branch" ] || continue
        id="$(new_change_id "$branch")"
        attempt="$(new_attempt_id "$branch")"
        sha="$(jq -r '.sha // empty' "$qf")"
        write_change_record "$branch" "$id" "$sha" '[]' queued "$attempt"
        retrofit_queued_change "$branch" "$id" "$attempt"
        printf '%s\n' "$id"
        return 0
    done
    last="$(jq -c --arg branch "$branch" \
        'select(.branch==$branch) | select(.event=="merged" or .event=="red" or .event=="timeout" or .event=="conflict" or .event=="blocked" or .event=="dropped")' \
        "$journal" 2>/dev/null | tail -1)"
    [ -n "$last" ] || return 1
    state="$(printf '%s\n' "$last" | jq -r .event)"
    sha="$(printf '%s\n' "$last" | jq -r '.sha // empty')"
    id="$(new_change_id "$branch")"
    attempt="$(new_attempt_id "$branch")"
    write_change_record "$branch" "$id" "$sha" '[]' "$state" "$attempt"
    printf '%s\n' "$id"
}

dependencies_are_acyclic() { # dependencies_are_acyclic <child-id> <after-json>
    local child="$1" after="$2"
    jq -en --arg child "$child" --argjson proposed "$after" \
        --slurpfile changes <(cat "$changes_dir"/*.json 2>/dev/null || true) '
        def parents($id): [$changes[] | select(.change_id==$id) | (.after // [])[]];
        def reaches($id; $target; $seen):
            if $id == $target then true
            elif ($seen | index($id)) != null then false
            else any(parents($id)[]; reaches(.; $target; $seen + [$id]))
            end;
        all($proposed[]; (reaches(.; $child; []) | not))
    ' >/dev/null
}

queue_entry_with_status() { # queue_entry_with_status <queue-file>
    # Queue files can be atomically renamed or consumed while a status
    # snapshot is being assembled. A vanished path is a retryable race.
    [ -f "$1" ] || return 1
    jq -n --slurpfile queue "$1" \
        --slurpfile changes <(cat "$changes_dir"/*.json 2>/dev/null || true) '
        def record($id): first($changes[] | select(.change_id==$id));
        def walk_dependencies($ids; $seen):
          $ids[] as $id |
          if ($seen | index($id)) != null then empty
          else (record($id) // {change_id:$id, branch:null, state:"missing", after:[]}) as $r |
            $r, walk_dependencies(($r.after // []); $seen + [$id])
          end;
        ($queue[0].after // []) as $after |
        [$after[] | . as $id | (record($id) // {}) as $r |
          {change_id:$id, branch:($r.branch // null), state:($r.state // "missing")}] as $deps |
        [walk_dependencies($after; [])] | unique_by(.change_id) as $all_deps |
        [$all_deps[] | select(.state != "merged" and
          (.state == "red" or .state == "timeout" or .state == "conflict" or
           .state == "blocked" or .state == "dropped" or .state == "missing"))] as $blocked |
        [$deps[] | select(.state != "merged" and
          (.state != "red" and .state != "timeout" and .state != "conflict" and
           .state != "blocked" and .state != "dropped" and .state != "missing"))] as $waiting |
        $queue[0] + {readiness:(if ($blocked|length)>0 then "blocked"
          elif ($waiting|length)>0 then "waiting" else "ready" end),
          dependencies:$deps, blocked_by:$blocked, waiting_on:$waiting}
    '
}

queue_readiness() { queue_entry_with_status "$1" | jq -r .readiness; }

# Status is observational, but it must remain cheap enough to use when the
# queue is large. Unlike scheduling (which asks about one candidate at a
# time), load every queued entry and change record once for the report.
queue_entries_with_status() {
    jq -n --slurpfile queue <(cat "$queue_dir"/*.json) \
        --slurpfile changes <(cat "$changes_dir"/*.json 2>/dev/null || true) '
        def record($id): first($changes[] | select(.change_id==$id));
        def walk_dependencies($ids; $seen):
          $ids[] as $id |
          if ($seen | index($id)) != null then empty
          else (record($id) // {change_id:$id, branch:null, state:"missing", after:[]}) as $r |
            $r, walk_dependencies(($r.after // []); $seen + [$id])
          end;
        $queue[] | . as $entry |
        (.after // []) as $after |
        [$after[] | . as $id | (record($id) // {}) as $r |
          {change_id:$id, branch:($r.branch // null), state:($r.state // "missing")}] as $deps |
        [walk_dependencies($after; [])] | unique_by(.change_id) as $all_deps |
        [$all_deps[] | select(.state != "merged" and
          (.state == "red" or .state == "timeout" or .state == "conflict" or
           .state == "blocked" or .state == "dropped" or .state == "missing"))] as $blocked |
        [$deps[] | select(.state != "merged" and
          (.state != "red" and .state != "timeout" and .state != "conflict" and
           .state != "blocked" and .state != "dropped" and .state != "missing"))] as $waiting |
        $entry + {readiness:(if ($blocked|length)>0 then "blocked"
          elif ($waiting|length)>0 then "waiting" else "ready" end),
          dependencies:$deps, blocked_by:$blocked, waiting_on:$waiting}
    '
}

holding_lock=0
holding_change_lock=0
change_owner_shell_pid=""
holding_coordinator_lock=0
coordinator_owner_shell_pid=""
migration_marker_active=0
release_migration_marker() {
    if [ "$migration_marker_active" -eq 1 ]; then
        rm -f "$root/scratch/merge-queue/migrating" "$root/state/merge-queue/migrating"
    fi
    migration_marker_active=0
}
# A priority intent counts as LIVE only while the coordinator keeps REFRESHING
# it. BUG-632: the old test was "the file exists", but its lifetime is the
# coordinator's entire wait, so no per-caller timeout could ever bound it.
# Refresh-based liveness separates "actively waiting" from "abandoned", which is
# what lets with-lock yield indefinitely to the former and still fail open on
# the latter.
intent_is_live() {
    local ipid mtime now
    [ -f "$lock_intent" ] || return 1
    ipid="$(cat "$lock_intent" 2>/dev/null || true)"
    case "$ipid" in ''|*[!0-9]*) return 1 ;; esac
    kill -0 "$ipid" 2>/dev/null || return 1
    mtime="$(stat -f %m "$lock_intent" 2>/dev/null || true)"
    [ -n "$mtime" ] || return 1
    now="$(date +%s)"
    [ "$((now - mtime))" -le "${MERGE_QUEUE_INTENT_STALE:-60}" ]
}

# Hand the lock OVER instead of racing for it. BUG-632: the releasing caller and
# the waiting coordinator both raced `mkdir`, and a with-lock caller polling at
# 5s routinely beat a coordinator polling at monitor_interval. Pausing past one
# coordinator poll makes the handoff deterministic. This only fires when someone
# ELSE releases while the coordinator waits -- the coordinator clears its own
# intent on acquire, so it never pauses itself.
release_lock() {
    if [ "$holding_lock" -eq 1 ]; then
        rm -rf "$lock"
        if [ "${lock_priority:-0}" != "1" ] && intent_is_live; then
            sleep "${MERGE_QUEUE_INTENT_HANDOFF:-5}"
        fi
    fi
    holding_lock=0
}
change_lock_owned() {
    [ "$holding_change_lock" -eq 1 ] \
        && [ "${BASHPID:-$$}" = "$change_owner_shell_pid" ] \
        && [ "$(cat "$change_lock/pid" 2>/dev/null || true)" = "$$" ]
}
release_change_lock() {
    if change_lock_owned; then
        rm -rf "$change_lock"
    fi
    holding_change_lock=0
}
acquire_change_lock() {
    while ! mkdir "$change_lock" 2>/dev/null; do
        local owner; owner="$(cat "$change_lock/pid" 2>/dev/null || true)"
        if [ -z "$owner" ]; then
            # The winner may be between mkdir and recording its pid.
            sleep 1
            owner="$(cat "$change_lock/pid" 2>/dev/null || true)"
            if [ -z "$owner" ] && rmdir "$change_lock" 2>/dev/null; then continue; fi
        fi
        if [ -n "$owner" ] && ! pid_is_alive "$owner"; then
            note "stealing stale change metadata lock (pid $owner is gone)"
            rm -rf "$change_lock"
            continue
        fi
        sleep 0.05
    done
    echo "$$" >"$change_lock/pid"
    holding_change_lock=1
    change_owner_shell_pid="${BASHPID:-$$}"
}
coordinator_lock_owned() {
    [ "$holding_coordinator_lock" -eq 1 ] \
        && [ "${BASHPID:-$$}" = "$coordinator_owner_shell_pid" ] \
        && [ "$(cat "$coordinator_lock/pid" 2>/dev/null || true)" = "$$" ]
}
release_coordinator_lock() {
    if coordinator_lock_owned; then
        rm -rf "$coordinator_lock"
    fi
    if [ "${BASHPID:-$$}" = "$coordinator_owner_shell_pid" ] \
        && [ "$(cat "$qdir/coordinator.pid" 2>/dev/null || true)" = "$$" ]; then
        rm -f "$qdir/coordinator.pid"
    fi
    holding_coordinator_lock=0
}
process_group_is_alive() {
    local pgid="${1:-}" error
    case "$pgid" in '' | *[!0-9]* | 0) return 1 ;; esac
    if error="$(kill -0 -- "-$pgid" 2>&1)"; then
        return 0
    fi
    case "$error" in
        *"Operation not permitted"* | *"operation not permitted"* | *"not permitted"*)
            return 0
            ;;
    esac
    return 1
}
terminate_gate_process_group() {
    local pgid="${1:-}" reason="${2:-abandoned gate}" term_grace="${3:-1}"
    process_group_is_alive "$pgid" || return 0
    note "terminating gate process group $pgid ($reason)"
    kill -TERM -- "-$pgid" 2>/dev/null || true
    sleep "$term_grace"
    process_group_is_alive "$pgid" || return 0
    kill -KILL -- "-$pgid" 2>/dev/null || true
}
reap_stale_gate_lock() {
    [ -d "$lock" ] || return 1
    local pid orphan_pgid
    pid="$(cat "$lock/pid" 2>/dev/null || true)"
    [ -n "$pid" ] && ! pid_is_alive "$pid" || return 1
    note "stealing stale gate lock (pid $pid is gone)"
    orphan_pgid="$(cat "$lock/gate_pgid" 2>/dev/null || true)"
    terminate_gate_process_group "$orphan_pgid" "stale gate lock owner $pid"
    rm -rf "$lock"
    return 0
}
cleanup() {
    if [ -n "$active_gate_pgid" ]; then
        terminate_gate_process_group "$active_gate_pgid" "coordinator exiting"
        active_gate_pgid=""
    fi
    release_migration_marker
    release_lock
    release_change_lock
    release_coordinator_lock
}
trap cleanup EXIT

# coordinator.lock is the lifetime singleton, while coordinator.pid remains the
# operator-facing pointer. Atomic mkdir closes the read-then-write race that let
# two starts both pass the old PID-file guard. A lock left by SIGKILL is stolen
# only after its recorded owner is proven dead (EPERM still counts as alive via
# pid_is_alive, matching the gate-lock safety rule).
acquire_coordinator_lock() {
    while ! mkdir "$coordinator_lock" 2>/dev/null; do
        local owner; owner="$(cat "$coordinator_lock/pid" 2>/dev/null || true)"
        if [ -z "$owner" ]; then
            # Another starter may be between mkdir and writing pid. Give it one
            # second; rmdir succeeds only if the directory is still empty, so
            # this also safely recovers a process killed in that tiny window.
            sleep 1
            owner="$(cat "$coordinator_lock/pid" 2>/dev/null || true)"
            if [ -z "$owner" ] && rmdir "$coordinator_lock" 2>/dev/null; then
                continue
            fi
        fi
        if [ -n "$owner" ] && ! pid_is_alive "$owner"; then
            note "stealing stale coordinator lock (pid $owner is gone)"
            rm -rf "$coordinator_lock"
            continue
        fi
        return 1
    done
    echo "$$" >"$coordinator_lock/pid"
    holding_coordinator_lock=1
    coordinator_owner_shell_pid="${BASHPID:-$$}"
}

# `exec` preserves the coordinator PID and does not run the old shell's EXIT
# trap. The new shell may therefore adopt the lifetime lock only when both
# persistent owner files already name its own PID and the old shell supplied a
# one-use marker. A separately started shell cannot satisfy the PID equality.
adopt_reexec_coordinator_lock() {
    [ "${MERGE_QUEUE_REEXEC_OWNER_PID:-}" = "$$" ] || return 1
    [ "$(cat "$coordinator_lock/pid" 2>/dev/null || true)" = "$$" ] || return 1
    [ "$(cat "$qdir/coordinator.pid" 2>/dev/null || true)" = "$$" ] || return 1
    holding_coordinator_lock=1
    coordinator_owner_shell_pid="${BASHPID:-$$}"
    unset MERGE_QUEUE_REEXEC_OWNER_PID
}

# Bash loads function bodies once, so a long-lived coordinator otherwise keeps
# using old queue policy after a script-only change lands. Reload only at the
# attempt boundary, with no gate process or short-lived lock active. The file
# we execute must be byte-for-byte the committed master blob; dirty checkout
# bytes and syntax-invalid updates stay dormant for operator recovery.
reexec_if_coordinator_updated() {
    local once="$1" committed_hash disk_hash
    [ "$once" -eq 0 ] || return 0
    [ -z "$active_gate_pgid" ] || return 0
    [ "$holding_lock" -eq 0 ] || return 0
    [ "$holding_change_lock" -eq 0 ] || return 0
    coordinator_lock_owned || return 0
    committed_hash="$(git -C "$root" rev-parse 'master:scripts/merge-queue.sh' 2>/dev/null || true)"
    [ -n "$committed_hash" ] || return 0
    [ "$committed_hash" != "$loaded_coordinator_hash" ] || return 0
    disk_hash="$(git -C "$root" hash-object "$coordinator_script" 2>/dev/null || true)"
    if [ "$disk_hash" != "$committed_hash" ]; then
        note "coordinator update is committed but working-tree script differs; deferring reload"
        return 0
    fi
    if ! bash -n "$coordinator_script"; then
        note "coordinator update failed bash -n; deferring reload"
        return 0
    fi
    note "reloading committed coordinator update (pid $$ stays constant)"
    record coordinator_reexec "" pid "$$" from "$loaded_coordinator_hash" to "$committed_hash"
    MERGE_QUEUE_REEXEC_OWNER_PID="$$" exec "$coordinator_script" run
}

coordinator_pid() {
    local pid
    pid="$(cat "$coordinator_lock/pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && pid_is_alive "$pid"; then echo "$pid"; return 0; fi
    pid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && pid_is_alive "$pid"; then echo "$pid"; return 0; fi
    return 1
}

# Legacy versions had no lifetime lock, so an unnamed detached loop can still
# be alive when the first fixed coordinator starts. Reap only sleeping Bash
# siblings whose command names this exact repository script; the Perl readiness
# parent also carries that script in argv and must not match. Never reap the
# PID-file keeper or gate-lock holder. Custom MERGE_QUEUE_STATE_DIR instances
# are skipped so an isolated test/dev queue cannot be mistaken for production.
coordinator_siblings() {
    [ -z "${MERGE_QUEUE_STATE_DIR:-}" ] || return 0
    ps -axo pid=,state=,comm=,command= 2>/dev/null | awk \
        -v self="$$" -v needle="$coordinator_script run" '
            $1 != self && $2 ~ /^[SI]/ && $3 ~ /(^|\/)bash$/ && index($0, needle) { print $1 }
        ' || true
}
reap_orphan_coordinators() {
    local keeper="$1" gate_pid candidate
    gate_pid="$(cat "$lock/pid" 2>/dev/null || true)"
    while IFS= read -r candidate; do
        [ -n "$candidate" ] || continue
        [ "$candidate" != "$keeper" ] || continue
        [ "$candidate" != "$gate_pid" ] || continue
        pid_is_alive "$candidate" || continue
        if kill -TERM "$candidate" 2>/dev/null; then
            note "reaped idle orphan coordinator pid $candidate (BUG-580)"
        else
            note "WARNING: found orphan coordinator pid $candidate but could not signal it"
        fi
    done < <(coordinator_siblings)
}

acquire_lock() { # acquire_lock <description> [branch] [log]
    local waited=0
    if [ "${lock_priority:-0}" = "1" ]; then
        printf '%s\n' "$$" >"$lock_intent" 2>/dev/null || true
    fi
    while ! mkdir "$lock" 2>/dev/null; do
        reap_stale_gate_lock && continue
        local pid; pid="$(cat "$lock/pid" 2>/dev/null || true)"
        if [ "$waited" -eq 0 ]; then
            note "gate lock held by pid ${pid:-?} ($(cat "$lock/what" 2>/dev/null || echo '?')); waiting"
            if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ] \
                && [ -n "${MERGE_QUEUE_TEST_LOCK_WAIT_MARKER:-}" ]; then
                : >"$MERGE_QUEUE_TEST_LOCK_WAIT_MARKER"
            fi
        fi
        waited=1
        # Keep the intent FRESH while genuinely waiting (BUG-632). with-lock
        # yields to a refreshed intent and fails open on a stale one, so this
        # pulse is the whole signal that separates a waiting coordinator from a
        # dead one. Rewriting the pid (rather than touch) also restores the file
        # if a failing-open caller removed it.
        if [ "${lock_priority:-0}" = "1" ]; then
            printf '%s\n' "$$" >"$lock_intent" 2>/dev/null || true
            sleep "${MERGE_QUEUE_INTENT_POLL:-2}"
        else
            sleep "$monitor_interval"
        fi
    done
    [ "${lock_priority:-0}" = "1" ] && rm -f "$lock_intent" 2>/dev/null
    echo "$$" >"$lock/pid"
    echo "$1" >"$lock/what"
    echo "${2:-}" >"$lock/branch"
    echo "${3:-}" >"$lock/log"
    date +%s >"$lock/started"
    holding_lock=1
}

ensure_gate_worktree() {
    if [ ! -d "$gate_wt" ]; then
        note "creating gate worktree at $gate_wt"
        git -C "$root" worktree add --detach "$gate_wt" master 1>&2
        [ -f "$root/scripts/worktree-warm.sh" ] && "$root/scripts/worktree-warm.sh" "$gate_wt" 1>&2 || true
    fi
    # Recover from a previous run that died mid-rebase.
    git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
}

# A checked-out master with tracked edits can make the final fast-forward fail
# after a successful, expensive gate. Untracked local state is intentionally
# excluded: it is common in the shared checkout and only blocks Git when the
# candidate would overwrite the same path.
# RFC-0159 C. Which contracts a diff touches, so a red rate is interpretable:
# a 29% red rate on compiler changes and on documentation changes are different
# facts. Deliberately coarse and additive -- a path may land in several classes,
# and an unmatched path contributes none.
classify_touches() { # classify_touches <newline-separated-paths>
    local paths="$1" out=""
    local pair name pattern
    for pair in \
        'typeck:^crates/witchy-types/' \
        'codegen:^crates/(witchy-lower/src/codegen|witchy-wir)/' \
        'analysis:^crates/witchy-lower/src/analysis\.rs$' \
        'interp:^crates/witchy-interp/' \
        'wasm-abi:^crates/witchy-runtime/' \
        'confinement:^crates/witchy-confinement/' \
        'std:^std/' \
        'docs:^(rfcs|book|spec|docs|wiki)/' \
        'queue-infra:^(scripts/(merge-queue|check|gate-report|test-for-paths|agent-check|test-agent-check-qos|submit-with-presubmit|test-submit-with-presubmit)\.sh|tools/merge-queue-tests/)' \
        ; do
        name="${pair%%:*}"
        pattern="${pair#*:}"
        if printf '%s\n' "$paths" | grep -qE "$pattern"; then
            out="$out,$name"
        fi
    done
    printf '%s' "${out#,}"
}

main_worktree_is_ready_to_land() {
    local current_branch
    current_branch="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
    [ "$current_branch" != "master" ] && return 0
    git -C "$root" diff --quiet --ignore-submodules -- \
        && git -C "$root" diff --cached --quiet --ignore-submodules --
}

# Why the main checkout currently blocks landing, as a one-line human reason —
# or empty when it does not block. The deferral above is otherwise INVISIBLE in
# `status`: a ready queue with a free gate lock looks idle, while the coordinator
# silently requeues the same entry every poll. Reported by `status` (as
# blocked_on) and `doctor` so a wedged queue is diagnosable without reading the
# coordinator log or counting journal `requeued` events.
main_worktree_block_reason() {
    main_worktree_is_ready_to_land && { printf ''; return 0; }
    local files
    # Tracked paths only, matching the predicate's two checks (unstaged +
    # staged). Named so the operator knows what to commit or move aside.
    files="$(git -C "$root" diff --name-only --ignore-submodules -- 2>/dev/null;
             git -C "$root" diff --cached --name-only --ignore-submodules -- 2>/dev/null)"
    files="$(printf '%s\n' "$files" | grep -v '^$' | sort -u | tr '\n' ' ')"
    printf 'main master checkout has tracked changes: %s' "${files% }"
}

# Is the gate's process group actively using CPU? `set -m` puts the gate in its
# own group whose pgid == the launched pid, so `ps -g <pgid>` lists the whole
# cargo/rustc/nextest tree. Sum their %cpu (a float across cores); "busy" means
# the gate is compiling or running tests, not hung — used to distinguish a silent
# WORKING gate from a silent WEDGED one. Fails safe: if ps yields nothing (group
# already gone, or an unexpected ps), returns false so the stall path proceeds.
# A gate that never produced output because another cargo held the package-cache
# or build-directory lock looks exactly like a slow or wedged candidate: the log
# is a handful of `Blocking waiting for file lock` lines and nothing else, and
# the attempt is recorded as a bare timeout. Measured 2026-09-20 over 2,390
# gates: 93 runs show that signature and 49 of them were recorded `timeout` or
# `red`, which is ~2% of all gates re-diagnosed as bad candidates. The stall
# itself is cargo's design and is NOT a median cost (recent stalled gates run
# ~9s slower than clean ones); naming it is what saves the next operator the
# misdiagnosis. Empty when the log does not carry the signature.
cargo_lock_starvation_note() { # cargo_lock_starvation_note <log>
    local log="$1" stalls signal
    [ -r "$log" ] || { printf ''; return 0; }
    stalls="$(grep -cF 'Blocking waiting for file lock' "$log" 2>/dev/null || true)"
    [ "${stalls:-0}" -gt 0 ] || { printf ''; return 0; }
    # "Signal" is every line that is not a lock-wait, a cargo progress line, or
    # blank: a starved stage has almost none, while a stage that merely waited
    # once and then ran has hundreds.
    signal="$(grep -vE 'Blocking waiting for file lock|^ *(Compiling|Downloading|Updating|Finished|Blocking)|^ *$' "$log" 2>/dev/null | grep -c . || true)"
    [ "${signal:-0}" -lt 60 ] || { printf ''; return 0; }
    printf 'cargo lock starvation: %s lock waits and only %s other output lines — another cargo held the package-cache/build lock for this stage' \
        "$stalls" "$signal"
}

group_is_busy() { # group_is_busy <pgid>
    local total
    total="$(ps -g "$1" -o %cpu= 2>/dev/null | awk '{s+=$1} END {print s+0}')"
    # Threshold well above idle jitter but far below one busy core (100%); a live
    # compile/test tree sits at hundreds of %.
    awk -v c="$total" 'BEGIN { exit !(c > 20) }'
}

# Paths that NO gate stage reads: documentation, plus the agent-instruction
# files that steer how agents work but that no test, build, or lint consumes
# (CLAUDE.md and the .claude/ and .codex/ trees -- skills, settings, hooks).
# Verified by grep: the only Rust mentions of CLAUDE.md and .claude/ are prose
# comments and a worktree fixture that builds its own temporary paths.
# Measured: a gate for an agent-skill edit ran the whole 4880-test suite for
# 235s and could not have observed the diff. Keep the classifier comment in
# check.sh in sync.
# glamour and coven/grimoire are not tested by this repo's gate (2026-09-24):
# a batch touching only coven-web (not embedded in the CLI) cannot change any
# stage.
# NOT here: projects/{glamour,coven,grimoire}/ -- eighteen of those files are
# include_str!'d into the witchy binary, so deleting or renaming one breaks
# the build and must still be gated.
GATE_NO_TEST_PATHS='^(rfcs/|wiki/|bugs/|docs/|external-refs/|scratch/|security-eval/|\.claude/|\.codex/|CLAUDE\.md$|AGENTS\.md$|tests/glamour\.rs$|tests/glamour/|src/example_tests/glamour\.rs$|src/example_tests/pm_coven\.rs$|src/source/package_inputs/registry_replay_tests\.rs$|src/commands/project_resolver_tests\.rs$|projects/coven-web/)'
# The cost report and its Python regression are not inputs to Cargo or product
# nextest. Gate exactly these paths with their regression on the rebased
# candidate; any other path keeps its existing gate scope.
COST_TOOL_PATHS='^scripts/(gate-test-cost|test-gate-test-cost)\.py$'
# The product nextest invocation parses and exercises `.config/nextest.toml`.
# It cannot change queue process/lock behavior, so do not append the unrelated
# queue fixture suite merely because test scheduling changed.
QUEUE_INFRA_PATHS='^(\.mbx\.toml|scripts/(check|gate-report|merge-queue|nextest-list-wrapper|state-paths|test-for-paths|test-impact|worktree-status|worktree-warm)\.(sh|py)|tools/merge-queue-tests/)'
QUEUE_INFRA_CORE_PATHS='^(\.mbx\.toml|scripts/(merge-queue|state-paths|check|test-for-paths|test-impact|gate-report)\.(sh|py)|tools/merge-queue-tests/)'

# Docs-safe path set, matching check.sh's WITCHY_GATE_SCOPE=docs classifier.
# RFCs have lightweight cross-link/status contracts in repo-tests; docs scope
# runs that package without taking the full compiler-gate lock.
# Empty input is not docs-safe (fail closed). rfcs/0167-witchy-language-contract.md is
# excluded because example_tests reads it. docs/ is the agent playbook —
# example_tests walks README/CONTRIBUTING/spec/book only.
diff_is_docs_safe() { # diff_is_docs_safe <newline-separated-paths>
    local paths="$1" unsafe
    [ -n "$paths" ] || return 1
    unsafe="$(printf '%s\n' "$paths" | grep -vE "$GATE_NO_TEST_PATHS" || true)"
    [ -z "$unsafe" ] || return 1
    if printf '%s\n' "$paths" | grep -cx 'rfcs/0167-witchy-language-contract\.md' >/dev/null; then
        return 1
    fi
    return 0
}

diff_is_cost_tool_only() { # diff_is_cost_tool_only <newline-separated-paths>
    local paths="$1" other
    [ -n "$paths" ] || return 1
    other="$(printf '%s\n' "$paths" | grep -vE "$COST_TOOL_PATHS" || true)"
    [ -z "$other" ]
}

diff_is_queue_infra_only() { # diff_is_queue_infra_only <newline-separated-paths>
    local paths="$1" non_queue_core
    [ -n "$paths" ] || return 1
    printf '%s\n' "$paths" | grep -cE "$QUEUE_INFRA_CORE_PATHS" >/dev/null || return 1
    non_queue_core="$(printf '%s\n' "$paths" \
        | grep -vE "${QUEUE_INFRA_CORE_PATHS}|^scripts/MERGE-QUEUE\.md$|${GATE_NO_TEST_PATHS}" || true)"
    [ -z "$non_queue_core" ]
}

diff_batch_scope_class() { # diff_batch_scope_class <newline-separated-paths>
    if diff_is_docs_safe "$1"; then
        printf 'docs\n'
    elif diff_is_queue_infra_only "$1"; then
        printf 'queue-infra\n'
    else
        printf 'product\n'
    fi
}

batch_scopes_are_compatible() { # batch_scopes_are_compatible <leader-class> <candidate-class>
    case "$1:$2" in
        docs:docs | queue-infra:queue-infra | product:product | product:docs) return 0 ;;
        *) return 1 ;;
    esac
}

# example_tests is compiled by the focused nextest selection. It cannot stale
# generated snapshots, rust-class results, or the workspace check/wasm
# legs' answers, so classifiers drop it before matching those surfaces.
diff_without_example_tests() { # diff_without_example_tests <newline-separated-paths>
    printf '%s\n' "$1" | grep -vE '^src/example_tests(\.rs|/)' || true
}

diff_product_compile_surface() { # prints matching lines
    diff_without_example_tests "$1" \
        | grep -E '^(crates/|src/|std/|build\.rs|Cargo\.(toml|lock)|\.cargo/|rust-toolchain)' || true
}

# rust-class --check rebuilds the witchy binary and reruns eight programs
# already covered by example_tests when a compiler crate changes. Keep the
# pre-merge leg for the harness, std, and the CLI; crate diffs rely on
# their focused nextest + package check, with --full/CI as backstop.
diff_rust_class_surface() {
    diff_without_example_tests "$1" \
        | grep -E '^(bench/rust-class/|src/|std/|build\.rs|Cargo\.(toml|lock)|\.cargo/|rust-toolchain)' || true
}

# Census TSV / spec/stdlib.md freshness tests live in the witchy package
# (tests/misc, example_sweeps). A crate-only nextest cannot observe that
# drift; regenerate only when this gate would run those tests or when the
# batch edits their inputs/outputs. Compiler crates rely on --full/CI.
diff_snapshot_surface() {
    diff_without_example_tests "$1" \
        | grep -E '^(src/|std/|examples/|projects/|benchmarks/|rfcs/0087-migration-census\.tsv|spec/stdlib\.md|tests/misc(\.rs|/)|build\.rs|Cargo\.(toml|lock)|\.cargo/|rust-toolchain)' || true
}

# Cargo profiles change code generation cost, not compiler semantics or either
# generated snapshot. Compare parsed manifests with their complete profile tree
# removed so comments, whitespace, and nested package overrides are handled
# without an error-prone diff-hunk heuristic.
cargo_manifest_change_is_profile_only() { # cargo_manifest_change_is_profile_only <base-sha>
    python3 - "$gate_wt" "$1" <<'PY'
import subprocess
import sys
import tomllib

worktree, base = sys.argv[1:]

def manifest(revision: str) -> dict:
    data = subprocess.check_output(
        ["git", "-C", worktree, "show", f"{revision}:Cargo.toml"]
    )
    parsed = tomllib.loads(data.decode())
    parsed.pop("profile", None)
    return parsed

raise SystemExit(manifest(base) != manifest("HEAD"))
PY
}

# witchy fmt --check walks std/, examples/, and projects/. The formatter
# itself is witchy_syntax::format, invoked from the CLI under src/.
diff_fmt_surface() {
    diff_without_example_tests "$1" \
        | grep -E '^(src/|std/|examples/|projects/|crates/witchy-syntax/)' || true
}

# Replay a green candidate onto a master tip that moved only by docs-safe
# paths. Prints the new HEAD sha. Returns 1 if the move is not docs-only or
# the replay conflicts — caller requeues and re-gates in that case.
try_replay_over_docs_only_master() { # try_replay_over_docs_only_master <old-base> <old-sha>
    local old_base="$1" old_sha="$2" new_master moved
    new_master="$(git -C "$root" rev-parse master)"
    [ "$new_master" != "$old_base" ] || { printf '%s\n' "$old_sha"; return 0; }
    moved="$(git -C "$root" diff --name-only --no-renames "$old_base..$new_master" 2>/dev/null || true)"
    diff_is_docs_safe "$moved" || return 1
    if ! git -C "$gate_wt" rebase --onto "$new_master" "$old_base" "$old_sha" >/dev/null 2>&1; then
        git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
        return 1
    fi
    git -C "$gate_wt" rev-parse HEAD
}

# Run the gate in its own process group with a stall monitor and optional
# emergency whole-gate timeout.
# Sets gate_result to "green", "red", or "timeout: <why>". Never returns nonzero.
gate_result=""
gate_attempt=0
# Sampled at the gate boundaries; empty for attempts that end before gating.
gate_load_start=""
gate_load_end=""
run_gate() { # run_gate <log> [fuzz-mode] [gate-scope] [queue-infra] [queue-infra-only] [cargo-target] [census-proof-sha] [skip-glamour]
    local log="$1"
    local progress_file="${log}.progress"
    local fuzz_mode="${2:-full}"
    local gate_scope="${3:-all}"
    local queue_infra="${4:-0}"
    local queue_infra_only="${5:-0}"
    local cargo_target_dir="${6:-target}"
    local census_proof_sha="${7:-}"
    local ungated="${8-glamour grimoire}" skip_sweeps="${9:-0}"
    local skip_book="${10:-0}"
    local nextest_args="${11:-}"
    local nextest_expr="${12:-}"
    local skip_rust_class="${13:-0}"
    local skip_compile="${14:-0}"
    local skip_wasm="${15:-0}"
    local check_packages="${16:-}"
    local skip_fmt="${17:-0}"
    local nextest_extra="${18:-}"
    local selected_gate_cmd="$gate_cmd"
    if [ "$queue_infra_only" -eq 1 ] && [ "$gate_cmd_is_default" -eq 1 ]; then
        selected_gate_cmd="./scripts/check.sh --queue-infra"
    fi
    local start; start="$(date +%s)"
    # `set -m` puts the background job in its own process group, so a timeout
    # can kill the WHOLE cargo/nextest tree, not just the top shell.
    #
    # Serialized gates deliberately bypass the globally configured sccache
    # wrapper. Detached coordinators can run in a sandbox where the wrapper
    # itself exits EPERM before Cargo metadata; exporting both Cargo controls
    # keeps daemon and foreground coordinators equivalent. Incremental is
    # left ON: check.sh already unsets CARGO_INCREMENTAL=0, and a prewarm
    # that built with incremental=0 / no test-strip forces the next gate to
    # rebuild every crate (measured: 13s incremental reuse vs 72s+ cold).
    set -m
    # Test execution runs at nextest's normal width. The macOS dyld stall that
    # once motivated NEXTEST_TEST_THREADS=4 here was a DISCOVERY problem —
    # CPU-count concurrent cold first-execs of ~100 MB --list binaries — and is
    # solved at the source by scripts/nextest-list-wrapper.sh
    # (.config/nextest.toml); by run time every binary is loader-warm. Bounding
    # execution as well doubled gate wall-clock (measured 2026-07-16: ~20.6 min
    # at width 4 vs the historical 8-10 min).
    # Keep an interactive-less macOS host awake while the serialized gate owns
    # the lock. Without this assertion, idle sleep suspends Cargo and the
    # watchdog together, so its wall-clock budget expires during maintenance
    # DarkWake even though the build received little CPU time. `caffeinate -i`
    # is scoped to the gate process and is absent on non-macOS hosts.
    local gate_awake=()
    if command -v caffeinate >/dev/null 2>&1; then
        gate_awake=(caffeinate -i)
    fi
    gate_dtest_report="${log}.dtest.json"
    rm -f "$progress_file" "$gate_dtest_report"
    ( cd "$gate_wt" && exec "${gate_awake[@]}" env -u CARGO_INCREMENTAL PATH="$gate_wt/scripts/bin:$PATH" "CARGO_TARGET_DIR=$cargo_target_dir" CARGO_PROFILE_TEST_STRIP=symbols RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= NEXTEST_STATUS_LEVEL=pass "WITCHY_GATE_PROGRESS_FILE=$progress_file" "WITCHY_GATE_FUZZ=$fuzz_mode" "WITCHY_GATE_SCOPE=$gate_scope" "WITCHY_GATE_QUEUE_INFRA=$queue_infra" "WITCHY_GATE_QUEUE_INFRA_ONLY=$queue_infra_only" "WITCHY_GATE_CENSUS_PROOF_SHA=$census_proof_sha" "WITCHY_GATE_UNGATED=$ungated" "WITCHY_GATE_PREFLIGHT_EXPR=$gate_preflight_expr" "WITCHY_GATE_PREFLIGHT_NEXTEST=$gate_preflight_nextest" "WITCHY_GATE_SKIP_SWEEPS=$skip_sweeps" "WITCHY_GATE_SKIP_BOOK=$skip_book" "WITCHY_GATE_NEXTEST=$nextest_args" "WITCHY_GATE_NEXTEST_EXPR=$nextest_expr" "WITCHY_GATE_NEXTEST_EXTRA=$nextest_extra" "WITCHY_GATE_DTEST_FROM=$gate_dtest_from" "WITCHY_GATE_DTEST_REPORT=$gate_dtest_report" "WITCHY_GATE_SKIP_RUST_CLASS=$skip_rust_class" "WITCHY_GATE_SKIP_COMPILE=$skip_compile" "WITCHY_GATE_SKIP_WASM=$skip_wasm" "WITCHY_GATE_SKIP_FMT=$skip_fmt" "WITCHY_GATE_CHECK_PACKAGES=$check_packages" bash -c "$selected_gate_cmd" ) >"$log" 2>&1 &
    local gpid=$!
    active_gate_pgid="$gpid"
    if [ "$holding_lock" -eq 1 ] \
        && [ "$(cat "$lock/pid" 2>/dev/null || true)" = "$$" ]; then
        printf '%s\n' "$gpid" >"$lock/gate_pgid"
    fi
    set +m
    local why=""
    # Liveness is measured from the last NON-heartbeat log write, NOT the file
    # mtime. check.sh emits a "still running (heartbeat …)" pulse every 120s so a
    # human tailing the log sees progress — but those synthetic writes must not
    # count as gate liveness. If they did (the old `stat -f %m` mtime signal),
    # a deadlocked/idle process tree would look fresh for the ENTIRE heartbeat
    # window: mtime never aged past stall_timeout, so the stall branch was never
    # entered and group_is_busy (the idle-vs-busy CPU test below) was never even
    # consulted until the pulses stopped. Observed 20260716-085448: 8 pulses ×
    # 120s = 16 min of a wedged tree before the watchdog began counting at all.
    # real_sig = count of log lines with heartbeat pulses removed; it only grows
    # on genuine output, so a change means real progress → reset the clock.
    local last_real_sig=""
    local last_progress_mtime=""
    local last_real_time="$start"
    local test_stage_seen=0
    local test_execution_start=""
    local test_stage_done=0
    local gate_yielded=0
    while :; do
        if ! pid_is_alive "$gpid"; then
            if wait "$gpid"; then gate_result="green"; else gate_result="red"; fi
            active_gate_pgid=""
            rm -f "$lock/gate_pgid" 2>/dev/null || true
            rm -f "$progress_file"
            return 0
        fi
        sleep "$monitor_interval"
        if [ -n "$gate_yield_probe" ] && "$gate_yield_probe"; then
            why="yielding to ready queue work"
            gate_yielded=1
            break
        fi
        local t; t="$(date +%s)"
        local elapsed=$((t - start))
        # Hard wall-clock on workspace TEST EXECUTION. Cargo compilation is
        # protected by the independent idle/busy-silence and optional whole-
        # gate ceilings below; charging it to this budget made a cold target
        # consume almost all 1,800 seconds before the first test ran. Start
        # only after nextest announces its run, which is the stable boundary
        # that its foreground Cargo build has completed. Streamed PASS lines
        # remain real liveness but cannot keep a starved test crawl alive.
        # 0 disables this execution budget.
        if [ "$test_stage_done" -eq 0 ]; then
            if [ "$test_stage_seen" -eq 0 ] \
                && grep -E '==> \[[0-9]+\] tests' "$log" >/dev/null 2>&1; then
                test_stage_seen=1
            fi
            if [ "$test_stage_seen" -eq 1 ]; then
                if grep -E '\[([0-9]+)\] tests .* took [0-9]+s' "$log" >/dev/null 2>&1 \
                    || { grep -E '==> \[[0-9]+\] ' "$log" 2>/dev/null \
                        | grep -vE '==> \[[0-9]+\] tests' >/dev/null; }; then
                    test_stage_done=1
                elif [ -z "$test_execution_start" ] \
                    && grep -aqF 'Nextest run ID ' "$log" 2>/dev/null; then
                    test_execution_start="$(awake_seconds)"
                elif [ -n "$test_execution_start" ] && [ "$workspace_test_budget" -gt 0 ]; then
                    local test_elapsed=$(( $(awake_seconds) - test_execution_start ))
                    if [ "$test_elapsed" -gt "$workspace_test_budget" ]; then
                        why="workspace test execution exceeded ${workspace_test_budget}s (MERGE_QUEUE_WORKSPACE_TEST_BUDGET)"
                        local starved; starved="$(cargo_lock_starvation_note "$log")"
                        [ -n "$starved" ] && why="$why — $starved"
                        break
                    fi
                fi
            fi
        fi
        # grep -c prints the count AND exits 1 when it is zero, so `|| echo 0`
        # would append a second value — use `|| true` and let the printed 0
        # stand (empty only if the log does not exist yet, which compares fine).
        local real_sig; real_sig="$(grep -vcF 'still running (heartbeat' "$log" 2>/dev/null || true)"
        if [ "$real_sig" != "$last_real_sig" ]; then
            last_real_sig="$real_sig"
            last_real_time="$t"
        fi
        local progress_mtime
        progress_mtime="$(stat -f %m "$progress_file" 2>/dev/null || true)"
        if [ -n "$progress_mtime" ] && [ "$progress_mtime" != "$last_progress_mtime" ]; then
            last_progress_mtime="$progress_mtime"
            last_real_time="$t"
        fi
        local age=$((t - last_real_time))
        if [ "$gate_timeout" -gt 0 ] && [ "$elapsed" -gt "$gate_timeout" ]; then
            why="gate exceeded ${gate_timeout}s (MERGE_QUEUE_GATE_TIMEOUT)"
            local starved; starved="$(cargo_lock_starvation_note "$log")"
            [ -n "$starved" ] && why="$why — $starved"
            break
        fi
        # Real-output silence alone is NOT a hang. The gate legitimately goes
        # quiet for minutes — from t+0, since tests are now the FIRST stage:
        # nextest compiles the `test` profile (separate artifacts from the
        # `dev`-profile build/clippy artifacts) and then enumerates+starts tests,
        # all before the first streamed `PASS` line — and under CPU contention that
        # silent window blew the 300s stall clock, killing HEALTHY gates (every
        # observed "no log output" timeout was this false positive, never a real
        # hang; the idle and busy-silence guards below backstop real wedges). So
        # gate liveness on CPU, not log writes: a process group burning CPU is
        # compiling/testing; only silence WITH no CPU is a genuine stall. A real
        # hang (deadlock/blocked syscall) consumes no CPU, so it still trips —
        # and now trips promptly, because heartbeats no longer keep `age` low.
        if [ "$age" -gt "$stall_timeout" ]; then
            # A busy group that has been silent for a NORMAL compile-window length
            # is fine (this is the false-positive fix). But a group that is busy
            # AND silent for far longer than any compile+enumeration takes is a
            # CPU-burning runaway (e.g. a busy-spin infinite loop in a test) — kill
            # it without relying on an arbitrary whole-suite duration.
            # `busy_silence_max` = 3× the active
            # stall window, comfortably above a cold test-profile compile or
            # bounded discovery under contention. An operator-provided
            # GATE_TIMEOUT can add a separate absolute ceiling.
            local busy_silence_max="${MERGE_QUEUE_BUSY_SILENCE_MAX:-$((stall_timeout * 3))}"
            if group_is_busy "$gpid" && [ "$age" -le "$busy_silence_max" ]; then
                continue
            fi
            if group_is_busy "$gpid"; then
                why="no log output for ${age}s despite a busy process group — runaway (MERGE_QUEUE_BUSY_SILENCE_MAX=${busy_silence_max})"
            else
                why="no log or discovery progress for ${age}s and process group idle (MERGE_QUEUE_STALL_TIMEOUT=${stall_timeout})"
            fi
            break
        fi
    done
    note "killing gate (pgid $gpid): $why"
    kill -TERM -- "-$gpid" 2>/dev/null || kill -TERM "$gpid" 2>/dev/null || true
    local terminate_grace=5
    # Deterministic fixtures use fake children that exit immediately on TERM;
    # they do not need to charge the production cleanup grace on every gate.
    if [ "${MERGE_QUEUE_ALLOW_TEST_ROOT:-0}" = "1" ]; then
        terminate_grace="${MERGE_QUEUE_TEST_TERMINATE_GRACE:-5}"
    fi
    sleep "$terminate_grace"
    kill -KILL -- "-$gpid" 2>/dev/null || true
    wait "$gpid" 2>/dev/null || true
    active_gate_pgid=""
    rm -f "$lock/gate_pgid" 2>/dev/null || true
    rm -f "$progress_file"
    # Once nextest has announced its run, Cargo finished the foreground
    # test-profile build and committed every fingerprint it will use. A
    # timeout in test execution cannot leave those files half-written. Keep
    # them: deleting all workspace fingerprints here turned one slow test run
    # into a guaranteed cold compiler rebuild on the retry (28m34s in the
    # 2026-09-24 incident). Before that boundary, a killed rustc may still be
    # writing an artifact, so retain the conservative invalidation.
    if grep -aqF 'Nextest run ID ' "$log" 2>/dev/null; then
        note "preserved workspace fingerprints in $gate_wt/$cargo_target_dir after test-execution timeout"
    else
        invalidate_workspace_fingerprints "$gate_wt/$cargo_target_dir"
    fi
    if [ "$gate_yielded" -eq 1 ]; then
        gate_result="yielded"
    else
        gate_result="timeout: $why"
    fi
    return 0
}

# `ld: symbol(s) not found` naming a `.llvm.<n>` symbol: an optimized crate's
# codegen units disagree about the hash suffix ThinLTO gave a promoted symbol,
# because an incremental build reused some units and regenerated others. It
# is the build cache's fault, not the candidate's (perf/interp-range-copy and
# perf/verdict-single-flight both went red this way on 2026-09-23 with no code
# that could cause it).
stale_incremental_link_failure() { # stale_incremental_link_failure <log>
    grep -aq 'symbol(s) not found' "$1" 2>/dev/null \
        && grep -aqE '\.llvm\.[0-9]+' "$1" 2>/dev/null
}

# Drop the incremental caches and fingerprints of this workspace's own crates
# so their next build starts clean; dependencies stay warm.
purge_workspace_incremental() { # purge_workspace_incremental <target-dir>
    local entry
    for entry in "$1"/debug/incremental/witchy*-* "$1"/debug/incremental/merge_queue_tests*-*; do
        [ -e "$entry" ] || continue
        rm -rf "$entry"
    done
    invalidate_workspace_fingerprints "$1"
}

# A kill can land while rustc is writing a workspace crate's output. Cargo
# names that output by package identity, not source content, and records a
# fingerprint only after a successful build -- so the half-written file sits
# under a fingerprint an EARLIER build left, and the next gate links it as
# fresh: `ld: symbol(s) not found` naming a `.llvm.*` or another branch's
# symbol, on a branch that did nothing wrong (twice on 2026-09-22/23). Drop
# the fingerprints of this workspace's own crates -- every gate that changes
# code rebuilds them anyway -- and keep every dependency's.
invalidate_workspace_fingerprints() { # invalidate_workspace_fingerprints <target-dir>
    local fingerprints="$1/debug/.fingerprint" entry
    [ -d "$fingerprints" ] || return 0
    for entry in "$fingerprints"/witchy-* "$fingerprints"/merge-queue-tests-*; do
        [ -e "$entry" ] || continue
        rm -rf "$entry"
    done
    note "invalidated workspace fingerprints in $1 after the kill"
}

cmd_submit() {
    local front=0
    # Bash 3.2 treats an empty array expansion as unbound under `set -u`.
    # Keep an empty sentinel; the loop and JSON conversion discard it.
    local parent_branches=("")
    while [ "${1:-}" != "" ]; do
        case "$1" in
            --front) front=1; shift ;;
            --after)
                [ -n "${2:-}" ] || { note "--after requires a parent branch"; exit 2; }
                parent_branches+=("$2"); shift 2
                ;;
            --) shift; break ;;
            -*) note "unknown submit option '$1'"; exit 2 ;;
            *) break ;;
        esac
    done
    local branch="${1:?usage: merge-queue.sh submit [--front] [--after <branch>]... <branch> [note]}"
    shift
    local msg="$*"
    if [ -f "$qdir/migrating" ]; then
        note "state migration is in progress; retry submit after it completes"
        exit 1
    fi
    git -C "$root" rev-parse --verify --quiet "refs/heads/$branch" >/dev/null \
        || { note "no local branch '$branch'"; exit 2; }
    local sha; sha="$(git -C "$root" rev-parse "refs/heads/$branch")"
    # Submit-time conflict pre-check (instant, in-memory): a branch that cannot
    # even merge with current master would burn a queue slot only to journal
    # `conflict` minutes later. git merge-tree does a real 3-way merge without
    # touching any worktree. Advisory-fail: refuse with the reason; --force to
    # override (e.g. master is about to change under you anyway).
    if [ "${MERGE_QUEUE_SKIP_PRECHECK:-}" != "1" ]; then
        if ! branch_merges_cleanly "$branch"; then
            note "REFUSED: $branch does not merge cleanly with current master — rebase it first"
            note "(the gate would only journal 'conflict'; MERGE_QUEUE_SKIP_PRECHECK=1 to submit anyway)"
            exit 1
        fi
    fi

    # Overlap warning is advisory and can run before the short metadata
    # critical section; branch editing and diffing never need a shared lock.
    # It is deliberately bounded: under a large queue a diff against every
    # pending branch makes submission itself unavailable. The coordinator still
    # performs the authoritative replay/conflict check before landing.
    local qf other overlap queue_count
    queue_count="$(find "$queue_dir" -maxdepth 1 -name '*.json' -type f -print | wc -l | tr -d ' ')"
    if [ "$queue_count" -le 96 ]; then
        for qf in "$queue_dir"/*.json; do
            [ -f "$qf" ] || continue
            other="$(jq -r .branch "$qf")"
            [ "$other" = "$branch" ] && continue
            git -C "$root" rev-parse --verify --quiet "refs/heads/$other" >/dev/null || continue
            overlap="$(comm -12 \
                <(git -C "$root" diff --name-only "master...$branch" 2>/dev/null | sort) \
                <(git -C "$root" diff --name-only "master...$other" 2>/dev/null | sort) \
                | head -5 | paste -sd' ' -)"
            if [ -n "$overlap" ]; then
                note "WARNING: overlaps queued '$other' on: $overlap"
            fi
        done
    else
        note "overlap warning skipped: queue has $queue_count pending changes (authoritative gate conflict check remains enabled)"
    fi

    # Shard receipt. `check.sh` writes one per COMMIT when a focused shard goes
    # green; its absence means this exact commit was never validated locally.
    #
    # Measured over 7d: 51% of all gate seconds went to attempts that never
    # landed, and 168 of 179 genuine reds -- 10.8h, 21% of ALL gate time --
    # failed in the `tests` stage, which `check.sh --fast` runs. Those gates
    # were spent rediscovering what a shard would have reported in the
    # author's own worktree, while every queued branch waited behind them.
    #
    # ADVISORY ONLY, deliberately. Refusing an unvalidated submission would be
    # a protocol change for every agent, and a warning first tells us how often
    # this actually fires before anything starts blocking.
    local tip_sha receipt_dir receipt_file
    local presubmit_shard="" presubmit_at="" presubmit_edit_elapsed_s="" presubmit_sha_match="false"
    tip_sha="$(git -C "$root" rev-parse "refs/heads/$branch" 2>/dev/null || true)"
    receipt_dir="$(witchy_merge_queue_state_dir "$root" 2>/dev/null || true)/receipts"
    receipt_file="$receipt_dir/$tip_sha.json"
    if [ -n "$tip_sha" ] && [ -f "$receipt_file" ]; then
        presubmit_sha_match="true"
        presubmit_shard="$(jq -r '((.shards // [.shard]) | map(select(. != null)) | join(",")) // ""' \
            "$receipt_file" 2>/dev/null || true)"
        presubmit_at="$(jq -r '.at // ""' "$receipt_file" 2>/dev/null || true)"
        presubmit_edit_elapsed_s="$(jq -r '.durations_s.edit // ""' "$receipt_file" 2>/dev/null || true)"
    fi
    # Suppress the warning where no shard is *possible*. A prose-only branch
    # cannot obtain a receipt -- test-for-paths reports no focused checks and a
    # docs-scope gate writes none -- so warning on every documentation change
    # trains submitters to ignore the warning, which costs exactly the branches
    # where it matters. Reuse the gate's own path classifier rather than a
    # second definition of "documentation".
    local submit_changed="" submit_docs_safe=0 presubmit_eligible="true"
    if [ -n "$tip_sha" ]; then
        local submit_base
        submit_base="$(git -C "$root" merge-base master "$tip_sha" 2>/dev/null || true)"
        if [ -n "$submit_base" ]; then
            submit_changed="$(git -C "$root" diff --name-only "$submit_base" "$tip_sha" 2>/dev/null || true)"
            if [ -n "$submit_changed" ] && diff_is_docs_safe "$submit_changed"; then
                submit_docs_safe=1
                presubmit_eligible="false"
            fi
        fi
    fi
    # Submission must stay cheap: the authoritative gate is already serialized,
    # while a surprise feature-graph rebuild here can keep a ready branch out of
    # the queue for hours. Clean linked worktrees may explicitly opt in to the
    # focused edit shard with MERGE_QUEUE_AUTO_PRESUBMIT=1. Never run it from a
    # dirty checkout: the shared master checkout may contain another agent's
    # compiler rewrite, and a dirty tree cannot mint an exact receipt. The
    # ordinary warning remains when the shard is not requested or fails.
    if [ -n "$tip_sha" ] && [ "$presubmit_sha_match" = "false" ] \
        && [ "$submit_docs_safe" -eq 0 ] \
        && [ "${MERGE_QUEUE_AUTO_PRESUBMIT:-0}" != "0" ]; then
        local submit_current_branch=""
        submit_current_branch="$(git -C "$here" branch --show-current 2>/dev/null || true)"
        if [ "$submit_current_branch" = "$branch" ] \
            && [ -x "$here/scripts/agent-check.sh" ] \
            && [ -z "$(git -C "$here" status --porcelain=v1 \
                --untracked-files=all --ignore-submodules 2>/dev/null)" ]; then
            note "no exact receipt for $branch@${tip_sha:0:9}; running focused pre-submit checks"
            if "$here/scripts/agent-check.sh" edit; then
                if [ -f "$receipt_file" ]; then
                    presubmit_sha_match="true"
                    presubmit_shard="$(jq -r '((.shards // [.shard]) | map(select(. != null)) | join(",")) // ""' \
                        "$receipt_file" 2>/dev/null || true)"
                    presubmit_at="$(jq -r '.at // ""' "$receipt_file" 2>/dev/null || true)"
                    presubmit_edit_elapsed_s="$(jq -r '.durations_s.edit // ""' \
                        "$receipt_file" 2>/dev/null || true)"
                fi
            else
                note "focused pre-submit checks failed; preserving advisory submission behavior"
            fi
        fi
    fi
    # RFC-0159 A. Actor identity. Recorded verbatim and never inferred; "unset"
    # is a first-class value so adoption is measurable rather than silently
    # defaulting to a wrong name. `by` stays $USER -- the machine account, not
    # the author.
    local actor="${MERGE_QUEUE_ACTOR:-unset}"
    local session="${MERGE_QUEUE_SESSION:-}"
    # A derived second opinion: an environment variable an agent forgets to set
    # is weak evidence, but the worktree path is structural. Kept DISTINCT from
    # `actor` on purpose -- one is verbatim, one is a guess -- so a disagreement
    # between them is visible instead of silently resolved.
    local actor_hint="unknown"
    case "$PWD" in
        */.claude/worktrees/*) actor_hint="claude" ;;
        */.codex/worktrees/*)  actor_hint="codex" ;;
        /private/tmp/witchy-*) actor_hint="linked-worktree" ;;
        "$root") actor_hint="master-checkout" ;;
    esac

    # RFC-0159 B. CLAUDE.md already mandates a Co-Authored-By trailer naming the
    # model; it appeared on 1.75% of commits. Warn and measure rather than
    # reject: refusing would be a protocol change for a convention nobody
    # follows yet, and the adoption number is what should decide whether it ever
    # becomes a refusal.
    local trailer_present="false"
    if [ -n "$tip_sha" ] && [ -n "$submit_base" ]; then
        if git -C "$root" log --format=%B "$submit_base..$tip_sha" 2>/dev/null \
            | grep -qi '^Co-Authored-By:'; then
            trailer_present="true"
        fi
    fi
    [ "$trailer_present" = "true" ] || \
        note "note: no Co-Authored-By trailer on this branch's commits (CLAUDE.md asks for one)"

    # RFC-0159 C. Change shape.
    local files_changed=0 insertions=0 deletions=0 touches=""
    if [ -n "$submit_base" ] && [ -n "$tip_sha" ]; then
        files_changed="$(printf '%s\n' "$submit_changed" | grep -c . || true)"
        local numstat
        numstat="$(git -C "$root" diff --numstat "$submit_base" "$tip_sha" 2>/dev/null || true)"
        insertions="$(printf '%s\n' "$numstat" | awk '$1 ~ /^[0-9]+$/ {n += $1} END {print n + 0}')"
        deletions="$(printf '%s\n' "$numstat" | awk '$2 ~ /^[0-9]+$/ {n += $2} END {print n + 0}')"
        touches="$(classify_touches "$submit_changed")"
    fi

    if [ -n "$tip_sha" ] && [ "$presubmit_sha_match" = "false" ] \
        && [ "$submit_docs_safe" -eq 0 ]; then
        note "WARNING: no green shard recorded for $branch@${tip_sha:0:9}."
        note "         From that clean branch, run './scripts/agent-check.sh edit',"
        note "         or './scripts/submit-with-presubmit.sh' (current branch),"
        note "         or './scripts/submit-with-presubmit.sh $branch',"
        if [ "$actor_hint" = "master-checkout" ]; then
            note "         shared master checkout: run the no-argument wrapper before resubmitting."
        fi
        note "         then resubmit the same tip (its queue position is preserved):"
        note "         a red gate here costs every queued branch a full serialized cycle."
    fi

    # A change ID is stable across updated SHAs and red-parent resubmissions.
    # Dependencies store IDs rather than branch names, so a child cannot become
    # accidentally ready merely because its parent's queue file was replaced.
    # The separate metadata lock is held only over these small JSON mutations;
    # it is unrelated to the heavyweight gate/merge lock.
    acquire_change_lock
    local change_id attempt_id new_generation=0 cf existing_after='[]' prior_state='' prior_sha='' parent parent_id parent_ids=("") added_after after
    cf="$(change_file_for_branch "$branch")"
    if change_id="$(change_id_for_branch "$branch" 2>/dev/null)"; then
        prior_state="$(jq -r '.state // empty' "$cf")"
        prior_sha="$(jq -r '.current_sha // empty' "$cf")"
        if [ "$prior_state" = merged ] && [ "$prior_sha" = "$sha" ]; then
            release_change_lock
            note "$branch change $change_id is already merged at submitted SHA $sha"
            return 0
        fi
        if [ "$prior_state" = merged ] || [ "$prior_state" = dropped ]; then
            # A branch name can be reused after a logical change is finished.
            # Keep the old ID-addressable record for existing descendants, but
            # give the new generation a fresh ID and dependency set.
            new_generation=1
            change_id="$(new_change_id "$branch")"
        else
            existing_after="$(jq -c '.after // []' "$cf")"
        fi
    else
        change_id="$(new_change_id "$branch")"
    fi
    for parent in "${parent_branches[@]}"; do
        [ -n "$parent" ] || continue
        [ "$parent" != "$branch" ] || { note "REFUSED: $branch cannot depend on itself"; exit 1; }
        if ! parent_id="$(known_parent_change_id "$parent")"; then
            note "REFUSED: dependency '$parent' has no known submission; submit it first"
            exit 1
        fi
        parent_ids+=("$parent_id")
    done
    added_after="$(printf '%s\n' "${parent_ids[@]}" | jq -Rsc 'split("\n") | map(select(length>0))')"
    after="$(jq -cn --argjson old "$existing_after" --argjson added "$added_after" '$old + $added | unique')"
    if ! dependencies_are_acyclic "$change_id" "$after"; then
        note "REFUSED: dependency update would create a cycle for $branch ($change_id)"
        exit 1
    fi
    [ "$new_generation" -eq 0 ] || archive_change_record "$branch"
    attempt_id="$(new_attempt_id "$branch")"
    write_change_record "$branch" "$change_id" "$sha" "$after" queued "$attempt_id"

    # Queue position is the filename's sort order. Normal submissions use
    # epoch seconds. --front uses a reverse timestamp ahead of both normal and
    # legacy front entries, and reprioritizes an already queued change.
    local stamp fname existing_qf="" tmp after_words front_qf
    after_words="$(jq -r 'join(" ")' <<<"$after")"
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        if [ "$(jq -r '.change_id // empty' "$qf")" = "$change_id" ] \
            || [ "$(jq -r '.branch // empty' "$qf")" = "$branch" ]; then
            if [ -z "$existing_qf" ]; then existing_qf="$qf"; else
                rm -f "$qf" "$qf.nobatch" "$qf.batch-limit"
            fi
        fi
    done
    if [ -n "$existing_qf" ]; then
        fname="$(basename "$existing_qf")"
        # A new SHA deserves a fresh batching decision. Markers describe the
        # previous content's failed gate, not the stable logical change.
        rm -f "$existing_qf.nobatch" "$existing_qf.batch-limit"
        tmp="$existing_qf.tmp.$$"
        jq -cn --arg branch "$branch" --arg ts "$(now)" --arg sha "$sha" \
            --arg by "${USER:-unknown}" --arg note "$msg" --arg id "$change_id" --arg attempt "$attempt_id" \
            --argjson after "$after" \
            '{schema:2, change_id:$id, attempt_id:$attempt, branch:$branch, sha:$sha, after:$after,
              submitted:$ts, by:$by, note:$note}' >"$tmp"
        mv "$tmp" "$existing_qf"
        record resubmitted "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$sha" after "$after_words" by "${USER:-unknown}" \
            presubmit_sha_match "$presubmit_sha_match" \
            presubmit_eligible "$presubmit_eligible" \
            presubmit_shard "$presubmit_shard" presubmit_at "$presubmit_at" \
            presubmit_edit_elapsed_s "$presubmit_edit_elapsed_s" \
            actor "$actor" actor_hint "$actor_hint" session "$session" \
            trailer_present "$trailer_present" files_changed "$files_changed" \
            insertions "$insertions" deletions "$deletions" touches "$touches"
        if [ "$front" -eq 1 ]; then
            front_qf="$queue_dir/$(front_stamp)-$(branch_key "$branch").json"
            mv "$existing_qf" "$front_qf"
            fname="$(basename "$front_qf")"
            note "updated queued change $change_id for $branch ($fname); moved to queue head"
        else
            note "updated queued change $change_id for $branch ($fname); queue position preserved"
        fi
    else
        stamp="$(date +%s)"
        [ "$front" -eq 1 ] && stamp="$(front_stamp)"
        fname="$stamp-$(branch_key "$branch").json"
        jq -cn --arg branch "$branch" --arg ts "$(now)" --arg sha "$sha" \
            --arg by "${USER:-unknown}" --arg note "$msg" --arg id "$change_id" --arg attempt "$attempt_id" \
            --argjson after "$after" \
            '{schema:2, change_id:$id, attempt_id:$attempt, branch:$branch, sha:$sha, after:$after,
              submitted:$ts, by:$by, note:$note}' >"$queue_dir/$fname"
        record submitted "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$sha" after "$after_words" by "${USER:-unknown}" \
            presubmit_sha_match "$presubmit_sha_match" \
            presubmit_eligible "$presubmit_eligible" \
            presubmit_shard "$presubmit_shard" presubmit_at "$presubmit_at" \
            presubmit_edit_elapsed_s "$presubmit_edit_elapsed_s" \
            actor "$actor" actor_hint "$actor_hint" session "$session" \
            trailer_present "$trailer_present" files_changed "$files_changed" \
            insertions "$insertions" deletions "$deletions" touches "$touches"
        note "queued $branch as $change_id ($fname)"
    fi
    release_change_lock
    # The coordinator refuses to gate against a dirty shared checkout and just
    # requeues, with a backoff, forever. That policy is right, but the only
    # signals were the coordinator's own log and `doctor`, which nobody reads
    # while submitting: the journal carries 1294 requeues on this cause since
    # 2026-07-24, and on 2026-09-20 one uncommitted file stalled every
    # candidate for about an hour. Tell the submitter at the moment they are
    # standing here, since they are the one agent certain to care.
    local blocked_now; blocked_now="$(main_worktree_block_reason)"
    if [ -n "$blocked_now" ]; then
        note "WARNING: the queue CANNOT gate until the shared checkout is clean"
        note "  $blocked_now"
        note "  Whoever owns that edit must commit it to a branch. Do not revert"
        note "  it: salvage it (worktree + git apply + submit) if it is not yours."
    fi
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    if [ -n "$cpid" ]; then
        note "coordinator (pid $cpid) will gate + merge it"
    else
        # Coordinator idle is the single largest latency contributor: over the
        # tail, changes waited orders of magnitude longer for a coordinator to
        # exist than they spent being gated. Start one rather than printing an
        # instruction the submitter has to act on.
        #
        # It MUST be the detached daemon, not a child of this shell. A
        # coordinator parented to a submitting agent's session dies when that
        # session ends -- which is exactly when an agent submits its last branch
        # and exits, recreating the gap this closes, intermittently and
        # invisibly. `daemon` already leaves the launcher's process group and
        # holds the same singleton lock, so this cannot produce a duplicate.
        if [ "${MERGE_QUEUE_AUTOSTART:-1}" = "1" ]; then
            note "no coordinator running; starting the detached daemon"
            if cmd_daemon; then
                local started; started="$(coordinator_pid 2>/dev/null || true)"
                record coordinator_started "$branch" pid "${started:-unknown}" \
                    origin "submit-autostart" actor "${MERGE_QUEUE_ACTOR:-unset}"
            else
                note "WARNING: could not start a coordinator; your submission will sit until one starts:"
                note "  ./scripts/merge-queue.sh daemon"
            fi
        else
            note "NO COORDINATOR RUNNING — your submission will sit until one starts:"
            note "  ./scripts/merge-queue.sh daemon"
        fi
    fi
}

# Shared by status (JSON) and doctor (prose): what is in flight right now?
inflight_vars() {
    lk_pid="$(cat "$lock/pid" 2>/dev/null || true)"
    lk_what="$(cat "$lock/what" 2>/dev/null || true)"
    lk_branch="$(cat "$lock/branch" 2>/dev/null || true)"
    lk_log="$(cat "$lock/log" 2>/dev/null || true)"
    lk_gate_pgid="$(cat "$lock/gate_pgid" 2>/dev/null || true)"
    lk_started="$(cat "$lock/started" 2>/dev/null || true)"
    lk_elapsed=""; lk_log_age=""; lk_stage=""
    local t; t="$(date +%s)"
    if [ -n "$lk_started" ]; then lk_elapsed=$((t - lk_started)); fi
    if [ -n "$lk_log" ] && [ -f "$lk_log" ]; then
        lk_log_age=$((t - $(stat -f %m "$lk_log")))
        lk_stage="$(current_stage "$lk_log")"
    fi
}

cmd_status() {
    inflight_vars
    local queue_json='[]' qf
    if ls "$queue_dir"/*.json >/dev/null 2>&1; then
        queue_json="$(queue_entries_with_status | jq -s .)"
    fi
    local blocked_on; blocked_on="$(main_worktree_block_reason)"
    local canary_json; canary_json="$(jq -c . "$canary_state" 2>/dev/null || true)"
    [ -n "$canary_json" ] || canary_json=null
    local canary_red=""
    [ ! -f "$canary_report" ] || canary_red="$canary_report"
    jq -n \
        --argjson q "$queue_json" \
        --argjson canary "$canary_json" --arg canary_red "$canary_red" \
        --slurpfile j <(tail -20 "$journal" 2>/dev/null || true) \
        --arg pid "$lk_pid" --arg gate_pgid "$lk_gate_pgid" \
        --arg what "$lk_what" --arg branch "$lk_branch" \
        --arg log "$lk_log" --arg stage "$lk_stage" \
        --arg elapsed "$lk_elapsed" --arg log_age "$lk_log_age" \
        --arg blocked_on "$blocked_on" \
        '{queue: $q,
          gate_lock: (if $pid == "" then null else
            {pid: $pid, gate_pgid: $gate_pgid, what: $what, branch: $branch, log: $log,
             stage: $stage, elapsed_s: $elapsed, log_age_s: $log_age} end),
          blocked_on: (if $blocked_on == "" then null else $blocked_on end),
          master_canary: $canary,
          master_canary_red: (if $canary_red == "" then null else $canary_red end),
          recent: $j}'
}

cmd_doctor() {
    echo "merge-queue doctor — $(now)"
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    if [ -n "$cpid" ]; then
        echo "coordinator : RUNNING (pid $cpid)"
        # Durability check: a coordinator started as `run` from an interactive or
        # tool-host session dies when that session's process group is reaped,
        # ORPHANING any in-flight gate (it holds no lock once dead, but its
        # check.sh keeps burning CPU with nothing left to merge the result).
        # A `daemon`-started coordinator is reparented to init (ppid 1) via
        # setsid and survives. Warn when it is NOT detached so an operator can
        # migrate it (kill + `daemon`) at the next idle moment.
        local cppid; cppid="$(ps -o ppid= -p "$cpid" 2>/dev/null | tr -d ' ' || true)"
        if [ -n "$cppid" ] && [ "$cppid" != 1 ]; then
            echo "  WARNING   : session-bound (ppid $cppid ≠ 1) — dies with its launching session and orphans the gate."
            echo "              Migrate when idle: kill $cpid && ./scripts/merge-queue.sh daemon"
        fi
    else
        echo "coordinator : NOT RUNNING${cpid:+ (last pid $cpid is dead)} — start (detached): ./scripts/merge-queue.sh daemon"
    fi
    local queue_files=() qf
    while IFS= read -r qf; do
        queue_files+=("$(basename "$qf")")
    done < <(find "$queue_dir" -maxdepth 1 -name '*.json' -type f -print | sort)
    local n="${#queue_files[@]}"
    if [ "$n" -gt 0 ]; then
        # Branch + readiness, not raw filenames. `status` has carried
        # per-change `readiness`/`blocked_by` all along, but `doctor` -- the
        # view an operator actually reads -- printed queue FILENAMES, so a
        # change stuck behind a failed parent was indistinguishable from one
        # about to gate. That costs real time: a dependency-blocked change sits
        # at the HEAD of the listing, which reads as "next up" rather than
        # "going nowhere until its parent lands".
        echo "queue       : $n pending"
        # Never let this listing fail `doctor`. It runs under `set -e` +
        # pipefail, and doctor is most often invoked precisely when the queue
        # metadata is malformed -- a diagnostic that dies on bad input is
        # useless. Compute into a variable, tolerate any error, fall back to
        # the raw filenames.
        local queue_lines=""
        queue_lines="$(queue_entries_with_status 2>/dev/null | jq -r '
            (.branch // .change_id // "unknown") as $b
            | (.readiness // "ready") as $r
            | (if $r == "blocked"
               then "  BLOCKED  \($b) — blocked by \([.blocked_by[]? | "\(.branch // .change_id) (\(.state))"] | join(", "))"
               elif $r == "waiting"
               then "  waiting  \($b) — after \([.waiting_on[]? | (.branch // .change_id)] | join(", "))"
               else "  ready    \($b)" end)' 2>/dev/null || true)"
        if [ -n "$queue_lines" ]; then
            printf '%s\n' "$queue_lines"
        else
            printf '  %s\n' "${queue_files[@]}"
        fi
    else
        echo "queue       : empty"
    fi
    # A pending queue that cannot land looks identical to a healthy idle one
    # unless this is said out loud.
    local blocked_on; blocked_on="$(main_worktree_block_reason)"
    if [ -n "$blocked_on" ]; then
        echo "landing     : BLOCKED — $blocked_on"
        echo "              The coordinator requeues instead of gating. Commit"
        echo "              those paths on a branch (they are someone's in-flight"
        echo "              work — do not discard them) or check out master clean."
    fi
    if [ -d "$lock" ]; then
        inflight_vars
        local health="ALIVE"
        if [ -z "$lk_pid" ] || ! pid_is_alive "$lk_pid"; then health="STALE (holder dead — next acquire steals it)"; fi
        echo "gate lock   : held by pid ${lk_pid:-?} — $health"
        echo "  what      : ${lk_what:-?}"
        if [ -n "$lk_gate_pgid" ]; then echo "  gate pgid : $lk_gate_pgid"; fi
        if [ -n "$lk_branch" ]; then echo "  branch    : $lk_branch"; fi
        if [ -n "$lk_elapsed" ]; then echo "  elapsed   : ${lk_elapsed}s (whole-gate timeout $(gate_timeout_display); test budget ${workspace_test_budget}s)"; fi
        if [ -n "$lk_log" ]; then
            echo "  log       : $lk_log"
            echo "  log age   : ${lk_log_age:-?}s since last output (stall kill at ${stall_timeout}s)"
            echo "  stage     : ${lk_stage:-(no stage marker yet)}"
        fi
    else
        # "free" is the single most misread line in this report. The coordinator
        # rebases and prepares each candidate WITHOUT the lock, so a healthy
        # coordinator shows a free lock for minutes at a stretch. Read together
        # with a pending queue that reads as "idle, and nothing is happening",
        # and the reflex is to restart the coordinator -- which kills the
        # in-flight gate, orphans its claim, and leaves the next reader looking
        # at the same picture. That loop cost 27 orphaned-claim recoveries and
        # 73 coordinator starts in a single day. So say what it is actually doing.
        echo "gate lock   : free (held only DURING a gate; rebase/prepare runs without it)"
        local last_log last_log_age
        last_log="$(find "$logs" -maxdepth 1 -name '*.log' -type f -print0 2>/dev/null \
            | xargs -0 ls -t 2>/dev/null | head -1 || true)"
        if [ -n "$last_log" ]; then
            last_log_age=$(( $(date +%s) - $(stat -f %m "$last_log") ))
            echo "  last gate : $(basename "$last_log") — last wrote ${last_log_age}s ago"
        fi
        local last_outcome
        last_outcome="$(grep -E '"event":"(merged|red|batch_red|timeout|conflict|blocked)"' "$journal" 2>/dev/null \
            | tail -1 | jq -r '"\(.event) \(.branch // "") at \(.ts)"' 2>/dev/null || true)"
        [ -n "$last_outcome" ] && echo "  last done : $last_outcome"
        if [ -n "$cpid" ]; then
            echo "  NOTE      : a live coordinator with a free lock is NORMAL between gates."
            echo "              Do NOT restart it to \"unstick\" the queue: that kills any"
            echo "              in-flight gate and orphans its claim. Confirm it is truly"
            echo "              idle first — gate log ages above are LOCAL time, the"
            echo "              journal is UTC."
        fi
    fi
    echo "test budget : ${workspace_test_budget}s (MERGE_QUEUE_WORKSPACE_TEST_BUDGET; 0 disables)"
    local canary_line
    canary_line="$(jq -r '"\(.result) at \(.sha[0:9]) (\(.at))"' "$canary_state" 2>/dev/null || true)"
    echo "canary      : ${canary_line:-none yet} (enabled=$canary_enabled, interval ${canary_interval}s)"
    [ ! -f "$canary_report" ] || echo "  MASTER RED: $canary_report"
    echo "recent      :"
    if [ -f "$journal" ]; then
        tail -5 "$journal" | jq -r '"  \(.ts)  \(.event)\t\(.branch)\(if .reason then "  (" + .reason + ")" else "" end)"'
    else
        echo "  (no journal yet)"
    fi
}

list_contains() { # list_contains <needle> <values...>
    local needle="$1" value; shift
    for value in "$@"; do [ "$value" = "$needle" ] && return 0; done
    return 1
}

change_connected_to_batch() { # change_connected_to_batch <change-id> <batch-change-ids...>
    local start="$1" targets; shift
    targets="$(printf '%s\n' "$@" | jq -Rsc 'split("\n") | map(select(length>0))')"
    jq -en --arg start "$start" --argjson targets "$targets" \
        --slurpfile changes <(cat "$changes_dir"/*.json 2>/dev/null || true) '
        def neighbors($id):
          ([$changes[] | select(.change_id==$id) | (.after // [])[]] +
           [$changes[] | select((.after // []) | index($id)) | .change_id]) | unique;
        def reaches($frontier; $seen):
          if any($frontier[]; . as $node | ($targets | index($node)) != null) then true
          elif ($frontier | length) == 0 then false
          else ([$frontier[] as $id | neighbors($id)[] as $neighbor |
                  select(($seen | index($neighbor)) == null) | $neighbor] | unique) as $next |
            reaches($next; ($seen + $frontier | unique))
          end;
        reaches([$start]; [])
    ' >/dev/null
}

stack_candidate_ready() { # stack_candidate_ready <queue-file> <batch-change-ids...>
    local qf="$1" cid dep state batch_ids=(""); shift
    batch_ids+=("$@")
    [ -f "$qf" ] || return 1
    cid="$(jq -r '.change_id // empty' "$qf")"
    [ -n "$cid" ] || return 1
    list_contains "$cid" "${batch_ids[@]}" && return 1
    # A root submission cannot be an explicit child of the current batch, but
    # it may itself be a parent of a queued tip. Avoid loading the entire
    # historical change graph for an unrelated root while retaining roots that
    # are referenced by another queued entry in the prospective stack.
    if ! jq -e '(.after // []) | length > 0' "$qf" >/dev/null 2>&1; then
        local referenced=0 child_qf
        for child_qf in "$queue_dir"/*.json; do
            [ -f "$child_qf" ] || continue
            [ "$child_qf" = "$qf" ] && continue
            if jq -e --arg parent "$cid" '(.after // []) | index($parent) != null' "$child_qf" >/dev/null 2>&1; then
                referenced=1
                break
            fi
        done
        [ "$referenced" -eq 1 ] || return 1
    fi
    while IFS= read -r dep; do
        [ -n "$dep" ] || continue
        list_contains "$dep" "${batch_ids[@]}" && continue
        state="$(change_state_for_id "$dep" 2>/dev/null || echo missing)"
        [ "$state" = merged ] || return 1
    done < <(jq -r '(.after // [])[]' "$qf")
    change_connected_to_batch "$cid" "${batch_ids[@]}"
}

# Replay only patches that are not already represented by the prepared tip.
# Starting from current master avoids checking out an old submitted tree and
# then rewriting every just-landed source file during rebase. That worktree
# churn invalidates Cargo's warm fingerprints even when the final bytes match.
# Return 2 when the target adds no patch, 1 on unsupported/conflicting history.
replay_unrepresented_patches() { # replay_unrepresented_patches <onto> <target>
    local onto="$1" target="$2" cherry_output merge_commits line mark commit
    local patches=()
    [ "$(git -C "$gate_wt" rev-parse HEAD 2>/dev/null || true)" = "$onto" ] || return 1
    git -C "$root" cat-file -e "$target^{commit}" 2>/dev/null || return 1
    merge_commits="$(git -C "$root" rev-list --merges "$onto..$target" 2>/dev/null)" \
        || return 1
    # Standard rebase does not preserve merge topology. Refuse merge commits
    # explicitly rather than silently dropping merge-only conflict resolutions.
    [ -z "$merge_commits" ] || return 1
    # Preserve the submitted SHA when it is already a direct descendant of the
    # prepared tip. Checkout writes only the actual branch delta, keeps normal
    # ancestry (so safe sweep can delete the ref), and avoids needless commit
    # rewriting. The patch-replay path is only for a submission on an older base.
    if git -C "$root" merge-base --is-ancestor "$onto" "$target" 2>/dev/null; then
        git -C "$gate_wt" checkout --detach --quiet "$target"
        return
    fi
    cherry_output="$(git -C "$root" cherry "$onto" "$target" 2>/dev/null)" || return 1
    while IFS=' ' read -r mark commit; do
        [ "$mark" = + ] && patches+=("$commit")
    done <<<"$cherry_output"
    [ "${#patches[@]}" -gt 0 ] || return 2
    git -C "$gate_wt" cherry-pick "${patches[@]}" >/dev/null 2>&1
}

submission_is_represented() { # submission_is_represented <submitted-sha>
    local submitted_sha="$1" cherry_status="" merge_commits=""
    [ -n "$submitted_sha" ] \
        && git -C "$root" rev-parse --verify --quiet "$submitted_sha^{commit}" >/dev/null \
        && merge_commits="$(git -C "$root" rev-list --merges "master..$submitted_sha" 2>/dev/null)" \
        && [ -z "$merge_commits" ] \
        && cherry_status="$(git -C "$root" cherry master "$submitted_sha" 2>/dev/null)" \
        && ! printf '%s\n' "$cherry_status" | grep -c '^+' >/dev/null
}

# Deterministic generated snapshots (the RFC-0087 census TSV and
# spec/stdlib.md) go stale whenever an unrelated branch
# lands first: the candidate regenerated them against an older master, the
# rebase keeps its now-incomplete snapshot, and the drift test turns a correct
# change into a ~28-min red gate plus a resubmission. After the candidate (and
# any batch) is fully prepared, re-run the two generators and, if the committed
# outputs drifted, commit the regenerated files onto the candidate as
# `chore(gate): re-baseline generated artifacts`. process_one captures the
# gated sha AFTER this step, so the amended sha is exactly what gets gated and
# fast-forwarded. Strict limits:
#   * ONLY the two whitelisted files are ever regenerated or committed;
#   * a generator BUILD or RUN failure never fails the candidate — regen is
#     skipped and the gate adjudicates as before;
#   * regen is skipped when the batch cannot make this gate observe
#     census/stdlib drift: docs-safe diffs, example_tests-only, and crate-only
#     compiler diffs (focused nextest does not run the snapshot tests).
#     `src/`, `std/`, corpus trees, and Cargo/toolchain still pay the
#     frontend-only generator build because that gate runs the freshness
#     tests. Match run_gate / check.sh (incremental on, test-strip=symbols):
#     flipping those flags in one shared target invalidates the artifacts and
#     rebuilds every crate.
rebaseline_generated_snapshots() { # rebaseline_generated_snapshots <base> <branch> <change-id> <attempt-id> <cargo-target>
    local rb_base="$1" rb_branch="$2" rb_change_id="$3" rb_attempt_id="$4"
    local rb_target="${5:-target}"
    local pre_diff unsafe tmp census_tmp census_err files="" census_run_ok=0 census_stderr_clean=0
    census_proof_ready=0
    pre_diff="$(git -C "$gate_wt" diff --name-only --no-renames "$rb_base..HEAD" 2>/dev/null || true)"
    [ -n "$pre_diff" ] || return 0
    unsafe="$(printf '%s\n' "$pre_diff" | grep -vE "$GATE_NO_TEST_PATHS" || true)"
    if [ -z "$unsafe" ] \
        && ! printf '%s\n' "$pre_diff" | grep -cx 'rfcs/0087-migration-census\.tsv' >/dev/null; then
        return 0
    fi
    # example_tests-only and crate-only compiler diffs cannot make this
    # gate observe census/stdlib drift (focused nextest). Skip the
    # generator cargo build; --full/CI still refresh and check.
    local snapshot_surface
    snapshot_surface="$(diff_snapshot_surface "$pre_diff")"
    if [ -z "$snapshot_surface" ]; then
        note "rebaseline: skipped generator build (batch cannot stale census/stdlib snapshots)"
        return 0
    fi
    if [ "$snapshot_surface" = "Cargo.toml" ] \
        && cargo_manifest_change_is_profile_only "$rb_base"; then
        note "rebaseline: skipped generator build (Cargo profile-only change cannot stale snapshots)"
        return 0
    fi
    # Never regenerate over TRACKED modifications (impossible after a clean
    # replay, but a stray edit must not be swept into the re-baseline commit).
    # Untracked files (target/, logs, progress sidecars) are normal in the
    # gate worktree and must not disable regen.
    [ -z "$(git -C "$gate_wt" status --porcelain --untracked-files=no 2>/dev/null)" ] || return 0
    # Clear the globally configured sccache wrapper exactly like run_gate does:
    # a detached daemon's sandbox EPERMs it, and a silently failing build here
    # would neuter the whole re-baseline path for daemon coordinators.
    if ! ( cd "$gate_wt" && env -u CARGO_INCREMENTAL PATH="$gate_wt/scripts/bin:$PATH" WITCHY_GATE_SCOPE=all "CARGO_TARGET_DIR=$rb_target" CARGO_PROFILE_TEST_STRIP=symbols RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= \
        cargo build -p witchy-snapshot-tools --bin rfc0087-census ) >/dev/null 2>&1; then
        note "rebaseline: generator build failed; skipping regen (the gate adjudicates)"
        return 0
    fi
    # scripts/bin/cargo redirects an isolated `-p` graph like this one into
    # `<target>-pure`, so `$rb_target/debug` can hold a STALE generator. On
    # 2026-09-26 that stale binary silently reverted a landed census-input
    # change. Run whichever copy the build just wrote.
    local census_bin="./$rb_target/debug/rfc0087-census"
    if [ -x "$gate_wt/$rb_target-pure/debug/rfc0087-census" ] \
        && { [ ! -e "$gate_wt/$census_bin" ] \
            || [ "$gate_wt/$rb_target-pure/debug/rfc0087-census" -nt "$gate_wt/$census_bin" ]; }; then
        census_bin="./$rb_target-pure/debug/rfc0087-census"
    fi
    tmp="$(mktemp "${TMPDIR:-/tmp}/witchy-rebaseline-XXXXXX")"
    census_tmp="$(mktemp "${TMPDIR:-/tmp}/witchy-census-proof-XXXXXX")"
    census_err="$(mktemp "${TMPDIR:-/tmp}/witchy-census-proof-error-XXXXXX")"
    # Each generator writes to a temp file first: a mid-run failure must never
    # truncate the committed snapshot, and an identical output must not dirty
    # the tree.
    if [ -f "$gate_wt/rfcs/0087-migration-census.tsv" ] \
        && ( cd "$gate_wt" && "$census_bin" . ) >"$census_tmp" 2>"$census_err"; then
        census_run_ok=1
        [ ! -s "$census_err" ] && census_stderr_clean=1
        if ! cmp -s "$census_tmp" "$gate_wt/rfcs/0087-migration-census.tsv"; then
            cat "$census_tmp" >"$gate_wt/rfcs/0087-migration-census.tsv"
            files="rfcs/0087-migration-census.tsv"
        fi
    fi
    if [ -f "$gate_wt/spec/stdlib.md" ] && ls "$gate_wt"/std/*.witchy >/dev/null 2>&1 \
        && ( cd "$gate_wt" && "$census_bin" --stdlib-docs std/*.witchy ) >"$tmp" 2>/dev/null \
        && ! cmp -s "$tmp" "$gate_wt/spec/stdlib.md"; then
        cat "$tmp" >"$gate_wt/spec/stdlib.md"
        files="${files:+$files }spec/stdlib.md"
    fi
    if [ -n "$files" ]; then
        # shellcheck disable=SC2086 — the whitelisted paths never contain spaces.
        if git -C "$gate_wt" add -- $files \
            && git -C "$gate_wt" commit --quiet -m "chore(gate): re-baseline generated artifacts"; then
            note "re-baselined generated artifacts for $rb_branch: $files"
            record rebaselined "$rb_branch" change_id "$rb_change_id" attempt_id "$rb_attempt_id" \
                files "$files" base "$rb_base"
        else
            note "rebaseline: commit failed; restoring the pristine candidate"
            git -C "$gate_wt" reset --quiet -- $files 2>/dev/null || true
            git -C "$gate_wt" checkout --quiet -- $files 2>/dev/null || true
        fi
    fi
    # The full gate's census test executes this same binary and requires this
    # same byte-for-byte stdout plus empty stderr. Preserve that successful
    # exact-candidate proof for reuse after the candidate SHA is captured. A
    # failed build/run, diagnostic, or failed rebaseline leaves no proof and
    # check.sh runs the test normally.
    if [ "$census_run_ok" -eq 1 ] && [ "$census_stderr_clean" -eq 1 ] \
        && cmp -s "$census_tmp" "$gate_wt/rfcs/0087-migration-census.tsv"; then
        census_proof_ready=1
    fi
    rm -f "$tmp" "$census_tmp" "$census_err"
    return 0
}

# Culprit eviction for an UNRELATED (non-stack) red batch. The historical
# behavior re-queued all N members for N individual full gates. Most red
# batches have one plausible culprit: the member whose diff touches the files
# the failure names. Parse the failing target out of the gate log (a nextest
# FAIL/TIMEOUT line, or a rustc `-->` / `could not compile` context), then
# score every member's own diff by name overlap with it. A unique positive
# top score evicts ONLY that member to an individual gate while the remaining
# N-1 re-gate together as ONE batch — 2 follow-up gates instead of N. Any
# ambiguity (no parsable target, nobody touches related files, or a tie)
# falls back to the split-all behavior. Soundness is untouched either way:
# eviction only changes which members re-gate TOGETHER; nothing lands without
# a green gate, and no member reaches a terminal red state except via its own
# solo gate (invariant 4).
evict_normalize() { printf '%s\n' "$1" | tr 'A-Z-' 'a-z_'; }

evict_candidate_index() { # evict_candidate_index <log> <base>; echoes "<index> <score>" on a clear signal
    local log="$1" ev_base="$2" plain fail_line rest="" target="" binary="" crate="" test_path="" err_path="" compile_crate=""
    plain="$(strip_ansi <"$log" 2>/dev/null || true)"
    [ -n "$plain" ] || return 0
    fail_line="$(printf '%s\n' "$plain" \
        | { grep -E '^[[:space:]]*(TRY [0-9]+ )?(FAIL|TIMEOUT|ABORT|SIGABRT|SIGSEGV|LEAK) \[' || true; } \
        | sed -n '1p')"
    if [ -n "$fail_line" ]; then
        # `FAIL [   1.234s] (NNN/MMMM) crate[::binary] test::path` — nextest's
        # progress counter sits between the timing bracket and the actual
        # crate/binary field. Strip it too, or `target`/`crate`/`binary` end
        # up parsing the counter itself (e.g. `(1065/2643)`) instead of the
        # real crate — a real bug found 2026-08-10: it silently produced
        # nonsense tokens on every invocation, so the culprit-eviction
        # heuristic's name-matching signal rarely fired as designed.
        # shellcheck disable=SC2046 — word splitting is the parse here.
        set -- $(printf '%s\n' "$fail_line" \
            | sed -E 's/^[[:space:]]*(TRY [0-9]+ )?[A-Z]+ \[[^]]*\][[:space:]]*(\([[:space:]]*[0-9]+[[:space:]]*\/[[:space:]]*[0-9]+[[:space:]]*\)[[:space:]]*)?//')
        target="${1:-}"; test_path="${2:-}"
        crate="${target%%::*}"
        binary="${target##*::}"
    fi
    err_path="$(printf '%s\n' "$plain" | sed -n 's/^[[:space:]]*--> \([^:][^:]*\):.*/\1/p' | sed -n '1p')"
    compile_crate="$(printf '%s\n' "$plain" | sed -n 's/^error: could not compile `\([^`]*\)`.*/\1/p' | sed -n '1p')"
    [ -n "$crate" ] || crate="$compile_crate"
    [ -n "$binary$err_path$crate" ] || return 0

    # Name tokens worth matching: the failing binary and the test path's
    # segments. Short/generic tokens (mod names like `tests`, the root crate)
    # only produce noise, so they are dropped.
    local tokens="" tok seg
    for tok in "$binary" "$crate"; do
        [ -n "$tok" ] || continue
        tok="$(evict_normalize "$tok")"
        case "$tok" in witchy | tests | test | main | src) continue ;; esac
        [ "${#tok}" -ge 4 ] || continue
        tokens="$tokens $tok"
    done
    rest="$test_path"
    while [ -n "$rest" ]; do
        seg="${rest%%::*}"
        if [ "$seg" = "$rest" ]; then rest=""; else rest="${rest#*::}"; fi
        [ -n "$seg" ] || continue
        seg="$(evict_normalize "$seg")"
        case "$seg" in witchy | tests | test | main | src) continue ;; esac
        [ "${#seg}" -ge 4 ] || continue
        tokens="$tokens $seg"
    done

    local best=0 best_index="" tie=0 mi f nf stem score subcrate_touched
    for mi in "${!batch_submitted_shas[@]}"; do
        score=0
        subcrate_touched=0
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            nf="$(evict_normalize "$f")"
            stem="${f##*/}"; stem="${stem%%.*}"; stem="$(evict_normalize "$stem")"
            if [ -n "$err_path" ] && [ "$f" = "$err_path" ]; then
                score=$((score + 4))
            fi
            for tok in $tokens; do
                if [ "$stem" = "$tok" ]; then
                    score=$((score + 3))
                else
                    case "$nf" in *"$tok"*) score=$((score + 1)) ;; esac
                fi
            done
            case "$crate" in
                "" | witchy)
                    if [ -n "$crate" ]; then
                        case "$f" in
                            src/* | tests/*) score=$((score + 1)) ;;
                            # The failing test's crate is the root `witchy` package,
                            # which links every workspace crate — a regression can
                            # legitimately come from ANY crates/*/src/* file, not just
                            # root src/tests/ (this is exactly how a `wasm_abi_catalog`
                            # surface test missed an `crates/witchy-interp/src/*`
                            # culprit in production, 2026-08-09). Recorded per-branch,
                            # not accumulated per file (see below), so a branch with a
                            # large diff can't outscore a small true culprit purely by
                            # file count.
                            crates/*/src/*) subcrate_touched=1 ;;
                        esac
                    fi
                    ;;
                *)
                    case "$f" in "crates/$crate/"*) score=$((score + 2)) ;; esac
                    ;;
            esac
        done < <(git -C "$root" diff --name-only --no-renames "$ev_base...${batch_submitted_shas[$mi]}" 2>/dev/null || true)
        # Flat, once-per-branch bonus (not per matching file): weak tie-breaking
        # signal only, same magnitude as the existing src/tests fallback above.
        # The per-file token/err_path matches remain the strong evidence.
        [ "$subcrate_touched" -eq 1 ] && score=$((score + 1))
        if [ "$score" -gt "$best" ]; then
            best="$score"; best_index="$mi"; tie=0
        elif [ "$score" -eq "$best" ] && [ "$best" -gt 0 ]; then
            tie=1
        fi
    done
    [ -n "$best_index" ] && [ "$best" -gt 0 ] && [ "$tie" -eq 0 ] || return 0
    printf '%s %s\n' "$best_index" "$best"
}

# An explicit --front submission is an operator signal that the queued change
# is more urgent than work which has not started its gate yet. Candidate replay
# and snapshot preparation deliberately happen without gate.lock and can take
# minutes, so rescan the live queue at the two admission boundaries below.
# Ordinary FIFO arrivals never preempt preparation.
ready_front_before() { # ready_front_before <selected-queue-file>
    local selected="$1" qf name branch
    while IFS= read -r qf; do
        [ "$qf" = "$selected" ] && return 1
        name="$(basename "$qf")"
        case "$name" in 00front-* | 0front-*) ;; *) continue ;; esac
        [ "$(queue_readiness "$qf")" = ready ] || continue
        branch="$(jq -r '.branch // empty' "$qf")"
        [ -n "$branch" ] || continue
        printf '%s\n' "$branch"
        return 0
    done < <(find "$queue_dir" -maxdepth 1 -name '*.json' -print | sort)
    return 1
}

process_one() { # process_one <queue-file>; returns 0 if the file was consumed
    local f="$1"
    # --front and resubmission may rename or replace a selected path. Rescan
    # the live queue instead of feeding a stale filename to jq.
    [ -f "$f" ] || return 1
    gate_load_start=""
    gate_load_end=""
    gate_preflight_expr=""
    gate_preflight_nextest=""
    gate_test_selector="legacy"
    gate_dtest_status="not_attempted"
    gate_dtest_elapsed_ms=""
    gate_dtest_changed_files=""
    gate_dtest_changed_definitions=""
    gate_dtest_selected_tests=""
    gate_dtest_executed_tests=""
    gate_dtest_selected_not_observed_tests=""
    gate_dtest_index_ms=""
    gate_dtest_index_load_ms=""
    gate_dtest_index_git_ms=""
    gate_dtest_index_stat_ms=""
    gate_dtest_index_read_ms=""
    gate_dtest_index_parse_ms=""
    gate_dtest_index_materialize_ms=""
    gate_dtest_index_graph_ms=""
    gate_dtest_index_serialize_ms=""
    gate_dtest_index_map_ms=""
    gate_dtest_selection_ms=""
    gate_dtest_selection_git_diff_ms=""
    gate_dtest_selection_old_sources_ms=""
    gate_dtest_selection_old_parse_us=""
    gate_dtest_selection_definition_compare_us=""
    gate_dtest_selection_old_cache_hits=""
    gate_dtest_selection_old_cache_misses=""
    gate_dtest_selection_old_cache_lookup_us=""
    gate_dtest_selection_old_cache_write_ms=""
    gate_dtest_selection_graph_walk_ms=""
    gate_dtest_selection_materialize_ms=""
    gate_dtest_partitions=""
    gate_dtest_overlay_hits=""
    gate_dtest_overlay_entries=""
    gate_dtest_overlay_compacted=""
    gate_dtest_index_overlay_entries=""
    gate_dtest_index_overlay_compacted=""
    gate_dtest_target_narrowed="false"
    gate_dtest_exact_filter=""
    gate_dtest_filter_bytes=""
    gate_dtest_required_filter_bytes=""
    gate_dtest_commands=""
    gate_dtest_reason=""
    gate_dtest_from=""
    gate_dtest_report=""
    gate_nextest_scheduled_tests=""
    gate_nextest_executed_tests=""
    gate_nextest_passed_tests=""
    gate_nextest_failed_tests=""
    gate_nextest_skipped_tests=""
    gate_nextest_binaries=""
    local branch change_id submitted_sha attempt_id
    branch="$(jq -r .branch "$f")"
    change_id="$(jq -r '.change_id // empty' "$f")"
    submitted_sha="$(jq -r '.sha // empty' "$f")"
    attempt_id="$(jq -r '.attempt_id // empty' "$f")"
    if ! git -C "$root" rev-parse --verify --quiet "refs/heads/$branch" >/dev/null; then
        note "branch $branch vanished; dropping"
        record dropped "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" reason "branch deleted"
        set_change_state "$change_id" dropped "$submitted_sha" "$attempt_id" || true
        consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id" && return 0
        return 1
    fi

    # A successful batch lands rebased commits while deliberately leaving each
    # submitter's branch ref untouched. The original SHA is therefore often not
    # an ancestor of master, and a stale duplicate queue file used to spend a
    # full gate proving an already-landed patch again. Pin the comparison to the
    # SHA stored at submission time so an agent advancing the branch cannot race
    # this check. `git cherry` compares patch IDs commit by commit: consume the
    # submission only when EVERY replayable commit is represented on master. One
    # `+` in a multi-commit branch keeps the whole submission queued. `git cherry`
    # intentionally ignores merge commits, so branches with an unlanded merge
    # commit also fail safe into the normal rebase + gate path, as does missing
    # or invalid queue metadata and any git error.
    if submission_is_represented "$submitted_sha"; then
        note "$branch is already represented on master; skipping duplicate gate"
        record already_merged "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" sha "$submitted_sha" \
            reason "all submitted patches already represented on master"
        set_change_state "$change_id" merged "$submitted_sha" "$attempt_id" || true
        if consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id"; then
            # Duplicate-heavy queues can contain hundreds of patch-equivalent
            # submissions after an integration tip lands. Sweeping every one
            # rescans the full journal and worktree list for each entry. Defer
            # reclamation to the next successful merge (or explicit `sweep`),
            # which coalesces all consumed duplicates into one scan.
            deferred_sweep=1
            return 0
        fi
        return 1
    fi

    # The coordinator singleton owns the dedicated gate worktree. `with-lock`
    # commands share compute/landing exclusion but never touch that worktree, so
    # checkout/rebase/batch preparation stays concurrent with an external gate.
    # The lock begins only after a candidate is fully prepared.
    local attempt_start; attempt_start="$(date +%s)"
    if ! queue_entry_matches "$f" "$change_id" "$submitted_sha" "$attempt_id"; then
        note "$branch was resubmitted before preparation started; selecting the updated SHA"
        return 1
    fi
    ensure_gate_worktree
    local base; base="$(git -C "$root" rev-parse master)"

    # Preserve Cargo freshness across candidate preparation. A retry starts by
    # checking this worktree back out at master and replaying the whole branch;
    # without this snapshot Git gives every branch-touched file a new mtime and
    # an assertion-only amendment recompiles the original broad patch. Restore
    # is blob-checked, so any source that actually changed retains its new mtime.
    local candidate_mtimes="$qdir/candidate-mtimes.json"
    rm -f "$candidate_mtimes"
    if [ -f "$here/build-seed.py" ]; then
        python3 "$here/build-seed.py" snapshot-mtimes "$gate_wt" \
            "$candidate_mtimes" >/dev/null 2>&1 || rm -f "$candidate_mtimes"
    fi

    # Build from the current master tree and replay only the submitted patches.
    # `process_one` is called from an `||` list, which disables Bash's implicit
    # errexit inside the function, so guard the correctness boundary explicitly:
    # a sandbox-denied index lock must never leave a stale checkout to be gated.
    if ! git -C "$gate_wt" checkout --detach --quiet "$base"; then
        rm -f "$candidate_mtimes"
        note "could not check out current master for $branch; refusing to gate the stale checkout"
        local checkout_failed; checkout_failed="$(date +%s)"
        record_attempt blocked "$branch" "$attempt_start" "$checkout_failed" \
            "$checkout_failed" "$checkout_failed" "$checkout_failed" \
            "$checkout_failed" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" base "$base" reason "candidate checkout failed"
        set_change_state "$change_id" blocked "$submitted_sha" "$attempt_id" || true
        consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id" && return 0
        return 1
    fi
    if ! replay_unrepresented_patches "$base" "$submitted_sha"; then
        git -C "$gate_wt" cherry-pick --abort >/dev/null 2>&1 || true
        rm -f "$candidate_mtimes"
        note "$branch does not replay cleanly onto master (or contains merge commits) — needs a human/agent rebase"
        local conflict_finished; conflict_finished="$(date +%s)"
        record_attempt conflict "$branch" "$attempt_start" "$conflict_finished" \
            "$conflict_finished" "$conflict_finished" "$conflict_finished" \
            "$conflict_finished" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" base "$base"
        set_change_state "$change_id" conflict "$submitted_sha" "$attempt_id" || true
        consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id" && return 0
        return 1
    fi
    # Cheap early check: a dirty shared checkout blocks landing regardless of
    # which candidate this is, so defer before claiming the entry for the gate.
    # This is an OPTIMISATION, not the guard -- the authoritative check stays
    # under gate.lock below, because another agent can dirty the checkout during
    # the rebase this skips, and dropping that one in favour of this one would
    # be a time-of-check/time-of-use race.
    #
    # It runs BEFORE `claim_queue_entry_for_gate` so a deferral never
    # transitions the change to `gating` -- a state it cannot proceed from and
    # which `cmd_drop` refuses. And it records the deferral with the SAME reason
    # the locked path uses: skipping the rebase is the saving, skipping the
    # journal entry would only hide the wait, and a ready queue with a free gate
    # lock already looks idle.
    if ! main_worktree_is_ready_to_land; then
        record requeued "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" sha "$submitted_sha" \
            reason "main master checkout has tracked changes before gate"
        return 2
    fi

    local urgent_branch=""
    if urgent_branch="$(ready_front_before "$f")"; then
        note "urgent front submission $urgent_branch arrived during replay; deferring $branch before claim"
        record requeued "$branch" change_id "$change_id" attempt_id "$attempt_id" \
            submitted_sha "$submitted_sha" sha "$submitted_sha" \
            reason "urgent front submission arrived during preparation" \
            urgent_branch "$urgent_branch"
        return 1
    fi

    # Preparation can take long enough for an operator drop or resubmission to
    # win. Revalidate both the queue attempt and its metadata transition before
    # admitting it to the gate; once state is `gating`, cmd_drop rejects it.
    if ! claim_queue_entry_for_gate "$f" "$change_id" "$submitted_sha" "$attempt_id"; then
        note "$branch changed or was dropped during preparation; discarding the stale candidate"
        return 1
    fi

    # BATCHING: stack further queued branches onto this candidate so ONE gate
    # validates them all. A branch joins the batch if it rebases CLEANLY onto
    # the stack — textual overlap that rebases fine is allowed (nearly every
    # language branch touches example_tests.rs; requiring disjoint files
    # forfeited batching exactly where queues run deepest). A red batch
    # either splits an ordered dependency stack at a prefix boundary or
    # re-queues unrelated members for individual gating. Nothing is merged
    # unvalidated and no member is blamed by association.
    local batch_files=("$f") batch_branches=("$branch") batch_ids=("$change_id")
    local batch_submitted_shas=("$submitted_sha")
    local batch_attempt_ids=("$attempt_id")
    local qf cand cand_id cand_attempt cdiff csha tip added stack_mode=0
    local batch_max="${MERGE_QUEUE_BATCH_MAX:-5}"
    local docs_batch_max="${MERGE_QUEUE_DOCS_BATCH_MAX:-25}"
    local batch_ceiling="$batch_max" batch_limit docs_batch_mode=0 initial_diff="" batch_scope_class=""
    case "$docs_batch_max" in '' | *[!0-9]* | 0) docs_batch_max="$batch_max" ;; esac
    # Raise only the documentation ceiling. The classifier is intentionally
    # stricter than gate_scope=docs: spec/book/README Markdown still receives
    # the full product gate, but may be integrated in one large compatible
    # batch. Any code/config/non-Markdown path keeps the semantic ceiling.
    initial_diff="$(git -C "$gate_wt" diff --name-only --no-renames "$base..HEAD" 2>/dev/null || true)"
    batch_scope_class="$(diff_batch_scope_class "$initial_diff")"
    if [ -n "$initial_diff" ] \
        && ! printf '%s\n' "$initial_diff" | grep -cEv '\.md$' >/dev/null; then
        docs_batch_mode=1
        batch_ceiling="$docs_batch_max"
    fi
    batch_limit="$batch_ceiling"
    if [ -f "$f.batch-limit" ]; then
        batch_limit="$(cat "$f.batch-limit" 2>/dev/null || echo 1)"
        case "$batch_limit" in '' | *[!0-9]*) batch_limit=1 ;; esac
        [ "$batch_limit" -ge 1 ] || batch_limit=1
        [ "$batch_limit" -le "$batch_ceiling" ] || batch_limit="$batch_ceiling"
    fi

    # Explicit dependency stacks take priority over unrelated opportunistic
    # batching. A child may join when every parent is already merged or is in
    # this candidate; repeated passes handle queue filenames that are not in
    # topological order. The final tip therefore validates the whole stack.
    if [ -n "$change_id" ] && [ ! -e "$f.nobatch" ]; then
        added=1
        while [ "$added" -eq 1 ] && [ "${#batch_branches[@]}" -lt "$batch_limit" ]; do
            added=0
            while IFS= read -r qf; do
                [ "$qf" = "$f" ] && continue
                [ -e "$qf.nobatch" ] && continue
                [ "${#batch_branches[@]}" -lt "$batch_limit" ] || break
                stack_candidate_ready "$qf" "${batch_ids[@]}" || continue
                cand="$(jq -r .branch "$qf")"
                cand_id="$(jq -r '.change_id // empty' "$qf")"
                cand_attempt="$(jq -r '.attempt_id // empty' "$qf")"
                list_contains "$cand" "${batch_branches[@]}" && continue
                csha="$(jq -r '.sha // empty' "$qf")"
                git -C "$root" cat-file -e "$csha^{commit}" 2>/dev/null || continue
                cdiff="$(git -C "$root" diff --name-only "master...$csha" 2>/dev/null | sort -u)"
                [ -n "$cdiff" ] || continue
                if [ "$docs_batch_mode" -eq 1 ] \
                    && printf '%s\n' "$cdiff" | grep -cEv '\.md$' >/dev/null; then
                    continue
                fi
                tip="$(git -C "$gate_wt" rev-parse HEAD)"
                if replay_unrepresented_patches "$tip" "$csha"; then
                    if claim_queue_entry_for_gate "$qf" "$cand_id" "$csha" "$cand_attempt"; then
                        batch_files+=("$qf"); batch_branches+=("$cand"); batch_ids+=("$cand_id")
                        batch_submitted_shas+=("$csha")
                        batch_attempt_ids+=("$cand_attempt")
                        stack_mode=1
                        added=1
                    else
                        git -C "$gate_wt" checkout --detach --quiet "$tip"
                    fi
                else
                    git -C "$gate_wt" cherry-pick --abort >/dev/null 2>&1 || true
                    git -C "$gate_wt" checkout --detach --quiet "$tip"
                fi
            done < <(find "$queue_dir" -maxdepth 1 -name '*.json' -print | sort)
        done
    fi

    # No explicit descendant joined: retain the existing throughput optimization
    # for independent READY changes. Waiting/blocked children can never sneak
    # into an unrelated batch and land ahead of a parent.
    if [ "$stack_mode" -eq 0 ] && [ ! -e "$f.nobatch" ]; then
        while IFS= read -r qf; do
            [ "$qf" = "$f" ] && continue
            [ -e "$qf.nobatch" ] && continue
            [ "${#batch_branches[@]}" -lt "$batch_limit" ] || break
            [ "$(queue_readiness "$qf")" = ready ] || continue
            cand="$(jq -r .branch "$qf")"
            cand_id="$(jq -r '.change_id // empty' "$qf")"
            cand_attempt="$(jq -r '.attempt_id // empty' "$qf")"
            list_contains "$cand" "${batch_branches[@]}" && continue
            csha="$(jq -r '.sha // empty' "$qf")"
            git -C "$root" cat-file -e "$csha^{commit}" 2>/dev/null || continue
            cdiff="$(git -C "$root" diff --name-only "master...$csha" 2>/dev/null | sort -u)"
            [ -n "$cdiff" ] || continue
            batch_scopes_are_compatible "$batch_scope_class" "$(diff_batch_scope_class "$cdiff")" \
                || continue
            if [ "$docs_batch_mode" -eq 1 ] \
                && printf '%s\n' "$cdiff" | grep -cEv '\.md$' >/dev/null; then
                continue
            fi
            tip="$(git -C "$gate_wt" rev-parse HEAD)"
            if replay_unrepresented_patches "$tip" "$csha"; then
                if claim_queue_entry_for_gate "$qf" "$cand_id" "$csha" "$cand_attempt"; then
                    batch_files+=("$qf"); batch_branches+=("$cand"); batch_ids+=("$cand_id")
                    batch_submitted_shas+=("$csha")
                    batch_attempt_ids+=("$cand_attempt")
                else
                    git -C "$gate_wt" checkout --detach --quiet "$tip"
                fi
            else
                git -C "$gate_wt" cherry-pick --abort >/dev/null 2>&1 || true
                git -C "$gate_wt" checkout --detach --quiet "$tip"
            fi
        done < <(find "$queue_dir" -maxdepth 1 -name '*.json' -print | sort)
    fi
    if [ "${#batch_branches[@]}" -gt 1 ]; then
        local batch_kind="batch"
        if [ "$docs_batch_mode" -eq 1 ]; then
            batch_kind="documentation batch"
        elif [ "$stack_mode" -eq 1 ]; then
            batch_kind="dependency stack"
        fi
        note "$batch_kind: gating ${#batch_branches[@]} branches at the tip: ${batch_branches[*]}"
    fi

    # Batch replay is part of candidate preparation too. Restore only after
    # the final tip is assembled, so a red A+B batch retried as A+B preserves
    # unchanged files from both branches rather than only the first member.
    if [ -f "$candidate_mtimes" ]; then
        python3 "$here/build-seed.py" restore-mtimes "$gate_wt" \
            "$candidate_mtimes" || true
        rm -f "$candidate_mtimes"
    fi

    # Re-baseline deterministic generated snapshots on the prepared candidate:
    # a snapshot gone stale under it costs ~2 min of prepare here instead of a
    # ~28-min red gate + resubmission. Runs BEFORE the sha capture and the diff
    # classification below, so the amended sha (and its spec/ paths) is what
    # gets classified, gated, and fast-forwarded.
    # One selector snapshot covers both preparation and the eventual gate. An
    # idle prewarm can promote only between attempts, never switch target dirs
    # underneath a candidate already being prepared.
    local cargo_target_dir; cargo_target_dir="$(gate_target_generation)"
    census_proof_ready=0
    rebaseline_generated_snapshots "$base" "$branch" "$change_id" "$attempt_id" \
        "$cargo_target_dir" || true

    # Snapshot generation is the longest unlocked preparation step. A newly
    # submitted --front change must not sit behind it and the ensuing gate.
    # Return every claimed member to queued before yielding to the urgent item.
    if urgent_branch="$(ready_front_before "$f")"; then
        note "urgent front submission $urgent_branch arrived during snapshot preparation; deferring ${batch_branches[*]}"
        local front_i
        for front_i in "${!batch_ids[@]}"; do
            if set_change_state "${batch_ids[$front_i]}" queued \
                "${batch_submitted_shas[$front_i]}" "${batch_attempt_ids[$front_i]}"; then
                record requeued "${batch_branches[$front_i]}" \
                    change_id "${batch_ids[$front_i]}" \
                    attempt_id "${batch_attempt_ids[$front_i]}" \
                    submitted_sha "${batch_submitted_shas[$front_i]}" \
                    sha "${batch_submitted_shas[$front_i]}" \
                    reason "urgent front submission arrived during preparation" \
                    urgent_branch "$urgent_branch"
            fi
        done
        return 1
    fi

    local sha; sha="$(git -C "$gate_wt" rev-parse HEAD)"

    # Positional compatibility for coordinators started before the canonical
    # interpreter and its differential corpus were retired.
    local fuzz_mode="none"
    local changed=""
    changed="$(git -C "$gate_wt" diff --name-only --no-renames \
        "$base..$sha" 2>/dev/null)" || changed=""

    # Gate scope from the same diff (see check.sh's WITCHY_GATE_SCOPE): if EVERY
    # changed path is documentation that no test or gate stage reads, the heavy
    # stages could only re-validate master's already-gated tree — skip them and
    # let post-merge CI's complete run be the backstop. The safe set is
    # deliberately TINY: rfcs/ (whose lightweight repo-tests contracts run in
    # docs scope, except rfcs/0167-witchy-language-contract.md, which
    # example_tests::public_sources_do_not_call_legacy_render_intrinsic reads —
    # it panics if that path vanishes), wiki/, bugs/, and docs/ (tracked, but
    # read by no test or gate stage — if a test ever starts reading them,
    # REMOVE them from this list), external-refs/ (vendored research notes,
    # never build inputs), and scratch//security-eval/ (gitignored). Everything
    # else — book/, spec/, README.md, std/, scripts/ except the two exact
    # cost-tool paths, .claude/, .github/, Cargo metadata — runs the full gate.
    # The cost-tool pair runs its Python regression on the candidate. Fail SAFE:
    # errored/empty diff -> all. Without --no-renames, a rename into the safe
    # set would hide the deletion side entirely.
    # (Capture-and-test rather than `grep -qv`: an inverted quiet match is the
    # one grep construct with divergent exit semantics across implementations,
    # e.g. a ugrep-shimmed PATH — and this decision gates merges.)
    local gate_scope="all"
    # Preparation already ran the exact RFC-0087 freshness proof. Reuse it
    # only when the proof protocol itself is unchanged in this candidate; the
    # candidate SHA is checked again inside check.sh. Missing or doubtful proof
    # simply retains the ordinary nextest test.
    local census_proof_sha=""
    if [ "$census_proof_ready" -eq 1 ] && [ -n "$changed" ] \
        && ! printf '%s\n' "$changed" \
            | grep -cE '^(\.config/nextest\.toml|Cargo\.(toml|lock)|scripts/(check|merge-queue)\.sh|tests/misc\.rs|tests/misc/rfc0087_migration_census\.rs)$' >/dev/null; then
        census_proof_sha="$sha"
    fi
    if [ -n "$changed" ] && diff_is_docs_safe "$changed"; then
        gate_scope="docs"
    elif [ -n "$changed" ] && diff_is_cost_tool_only "$changed"; then
        gate_scope="cost-tool"
    fi

    # Corpus sweeps from the same diff (see check.sh's WITCHY_GATE_SKIP_SWEEPS).
    # The sweeps walk every example, std module and documented ```witchy block
    # on both backends — ~900 compile+run cycles, the largest remaining block
    # of work in the suite. They are keyed on their INPUTS, never on whether
    # examples/ changed: a compiler change over an untouched corpus is the case
    # they exist to catch, so `examples/ is unchanged` is NOT a licence to skip.
    #
    # A sweep can only differ if the batch touches something it reads. Anything
    # outside this list — tests/, scripts/, .github/, .claude/ — cannot move the
    # result, and re-deriving a known answer costs a full corpus pass.
    #
    # Fail SAFE toward CORRECTNESS here, unlike the ungated-project list above:
    # an errored or empty diff RUNS the sweeps. They guard the prime directive,
    # so an unnecessary run costs time while a wrongly skipped one costs the
    # parity guarantee. Any doubt about a path belongs on this list.
    local skip_sweeps=0
    if [ -n "$changed" ] \
        && [ -z "$(diff_without_example_tests "$changed" | grep -E '^(crates/|src/|std/|examples/|book/|spec/|README\.md|build\.rs|Cargo\.(toml|lock)|\.cargo/|rust-toolchain)' || true)" ]; then
        skip_sweeps=1
    fi

    # check.sh no longer has a runnable-book leg or a wasm playground leg
    # (consumers are untested by this repo since 2026-09-24). The positional
    # skip_book/skip_wasm/ungated values below are inert plumbing kept only so
    # the run_gate argument order stays stable.
    local skip_book=1

    # RFC-0111 rust-class, workspace check, and the wasm playground
    # only move when their own surfaces move. example_tests is compiled by
    # the focused nextest selection. Interpreter-only diffs skip rust-class
    # and wasm. Fail-safe: empty/errored diff keeps them.
    local skip_rust_class=0 skip_compile=0 skip_wasm=0
    if [ -n "$changed" ] && [ -z "$(diff_product_compile_surface "$changed")" ]; then
        skip_compile=1
    fi
    if [ -n "$changed" ] && [ -z "$(diff_rust_class_surface "$changed")" ]; then
        skip_rust_class=1
    fi
    skip_wasm=1
    local skip_fmt=0
    if [ -n "$changed" ] && [ -z "$(diff_fmt_surface "$changed")" ]; then
        skip_fmt=1
    fi

    # Serialized-gate nextest selection: reuse test-for-paths.sh's crate /
    # binary mapping. Fail-safe to an unfiltered --workspace on any doubt
    # (old mapper, empty output, unknown path). --full / standalone ignore
    # this and keep the complete suite.
    local nextest_args="" nextest_expr="" nextest_extra="" nextest_sel="" check_packages="" mapper
    mapper="$gate_wt/scripts/test-for-paths.sh"
    [ -f "$mapper" ] || mapper="$here/scripts/test-for-paths.sh"
    if [ -n "$changed" ] && [ -f "$mapper" ]; then
        nextest_sel="$(printf '%s\n' "$changed" | bash "$mapper" --gate-nextest 2>/dev/null || true)"
    fi
    case "$(printf '%s\n' "$nextest_sel" | sed -n '1p')" in
        -p\ * | --test\ *)
            nextest_args="$(printf '%s\n' "$nextest_sel" | sed -n '1p')"
            nextest_expr="$(printf '%s\n' "$nextest_sel" | sed -n '2p')"
            check_packages="$(printf '%s\n' "$nextest_sel" | sed -n '3p')"
            nextest_extra="$(printf '%s\n' "$nextest_sel" | sed -n '4p')"
            ;;
        *)
            nextest_args=""
            nextest_expr=""
            check_packages=""
            nextest_extra=""
            ;;
    esac

    # Let dtest select and launch nextest inside check.sh. The mapper still
    # owns the conservative fallback, non-Rust/workspace routes, secondary
    # feature partitions, queue fixtures, and package checks. A report is read
    # only after the gate for telemetry; it no longer controls execution.
    # A second feature partition gets its own dtest run inside check.sh.
    if [ -n "$nextest_args" ] && [ -n "$changed" ] \
        && printf '%s\n' "$changed" | grep -cE '\.rs$' >/dev/null; then
        gate_dtest_status="pending"
        gate_dtest_from="$base"
        gate_test_selector="dtest"
    elif [ -z "$nextest_args" ]; then
        gate_dtest_status="skipped_workspace"
        gate_dtest_reason="legacy mapper required workspace"
    else
        gate_dtest_status="skipped_non_rust"
        gate_dtest_reason="no changed Rust source"
    fi

    local ungated="glamour grimoire"

    # Queue fixtures manipulate process groups, detached daemons, file locks,
    # and nested Git repositories. Run that binary in check.sh's isolated,
    # serial shard only when this batch can change the queue substrate. The
    # ordinary product suite always excludes it, avoiding load-induced false
    # reds without weakening validation of relevant infrastructure changes.
    local queue_infra=0
    if [ -n "$changed" ] \
        && printf '%s\n' "$changed" | grep -cE "$QUEUE_INFRA_PATHS" >/dev/null; then
        queue_infra=1
    fi

    # Queue-core and check.sh / path-router changes are validated by the
    # process-isolated fixture shard, not by re-running an unrelated product
    # tree already green on master. Documentation may ride along. Nextest
    # config and worktree tooling can still affect general product-gate
    # behavior and therefore retain the complete gate.
    local queue_infra_only=0
    if [ "$queue_infra" -eq 1 ] && diff_is_queue_infra_only "$changed"; then
        queue_infra_only=1
    fi
    # Queue-infrastructure-only gates run their isolated fixture shard rather
    # than check.sh's product nextest command, so dtest is intentionally not
    # invoked even when the changed queue fixture is Rust. Record that as a
    # skip, not as a failed dtest selection/fallback.
    if [ "$queue_infra_only" -eq 1 ]; then
        gate_dtest_status="skipped_queue_infra"
        gate_dtest_reason="queue-infra-only gate bypasses dtest"
        gate_test_selector="legacy"
    fi

    gate_attempt=$((gate_attempt + 1))
    local log; log="$logs/$(date +%Y%m%d-%H%M%S)-$(branch_key "$branch")-$$-$gate_attempt.log"
    local prepare_finished; prepare_finished="$(date +%s)"
    local lock_what="full gate: $branch"
    if [ "${#batch_branches[@]}" -gt 1 ]; then
        lock_what="${batch_kind:-batch}: ${batch_branches[*]}"
    fi
    # Docs-safe landings cannot invalidate a concurrent code gate and do not
    # contend with one. They skip gate.lock so an RFC/wiki/bugs landing does
    # not sit behind a 10-minute (or starved 70-minute) product suite.
    local lock_acquired
    if [ "$gate_scope" = "docs" ]; then
        note "docs-safe $branch: landing without the full-gate lock"
        lock_acquired="$(date +%s)"
    else
        lock_priority=1 acquire_lock "$lock_what" "$branch" "$log"
        lock_acquired="$(date +%s)"
    fi

    # Queue submission remains concurrent with candidate preparation. Re-check
    # the immutable attempt selected above before spending a full gate on it.
    local qi stale_attempt=0
    for qi in "${!batch_files[@]}"; do
        queue_entry_matches "${batch_files[$qi]}" "${batch_ids[$qi]}" \
            "${batch_submitted_shas[$qi]}" "${batch_attempt_ids[$qi]}" \
            || stale_attempt=1
    done
    if [ "$stale_attempt" -eq 1 ]; then
        note "$branch or a batched member was resubmitted during preparation; rebuilding the candidate"
        local stale_finished; stale_finished="$(date +%s)"
        release_lock
        record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$stale_finished" "$stale_finished" "$stale_finished" \
            change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
            sha "$sha" reason "submission changed before gate"
        for qi in "${!batch_ids[@]}"; do
            set_change_state "${batch_ids[$qi]}" queued "${batch_submitted_shas[$qi]}" \
                "${batch_attempt_ids[$qi]}" || true
        done
        return 1
    fi

    # Preparation raced the shared lock by design. Validate its base after
    # acquisition; if another validated landing moved master, release without
    # gating and rebuild the candidate from the new base on the next loop.
    if [ "$(git -C "$root" rev-parse master)" != "$base" ]; then
        note "master moved while $branch waited for the gate lock; re-preparing"
        local pre_gate_requeued; pre_gate_requeued="$(date +%s)"
        release_lock
        record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$pre_gate_requeued" "$pre_gate_requeued" \
            "$pre_gate_requeued" change_id "$change_id" sha "$sha" \
            reason "master moved before gate"
        local pri; for pri in "${!batch_ids[@]}"; do
            set_change_state "${batch_ids[$pri]}" queued "${batch_submitted_shas[$pri]}" \
                "${batch_attempt_ids[$pri]}" || true
        done
        return 1
    fi

    # Do this under gate.lock, immediately before the expensive operation. A
    # dirty main master checkout is an operational wait, not a bad submission:
    # preserve every queue entry and retry once its owner has committed or
    # stashed the tracked changes. Return 2 so `run --once` reports the wait
    # instead of spinning on the same ready entry.
    if ! main_worktree_is_ready_to_land; then
        note "main master checkout has tracked changes; deferring $branch before the full gate"
        local dirty_finished; dirty_finished="$(date +%s)"
        release_lock
        local dqi; for dqi in "${!batch_ids[@]}"; do
            set_change_state "${batch_ids[$dqi]}" queued "${batch_submitted_shas[$dqi]}" \
                "${batch_attempt_ids[$dqi]}" || true
        done
        record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$dirty_finished" "$dirty_finished" "$dirty_finished" \
            change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
            sha "$sha" reason "main master checkout has tracked changes before gate"
        return 2
    fi

    # Replay whatever red the previous attempt for this change, first.
    # Queue-infrastructure tests assert the gate's own stage ordering. Replaying
    # a stale product-test preflight into that focused shard changes the thing
    # those fixtures measure; product gates retain the normal failure-first
    # preflight optimization.
    if [ "$queue_infra_only" -eq 1 ]; then
        gate_preflight_expr=""
        gate_preflight_nextest=""
    else
        gate_preflight_expr="$(preflight_expr_for "$change_id" || true)"
        gate_preflight_nextest="$(preflight_nextest_for "$change_id" || true)"
    fi
    [ -n "$gate_preflight_expr" ] && note "preflight: replaying last attempt's failures first"
    note "gating $branch (rebased to $sha on $base; target=$cargo_target_dir; fuzz=$fuzz_mode; scope=$gate_scope; queue-infra=$queue_infra; queue-infra-only=$queue_infra_only; ungated=[$ungated]; skip-sweeps=$skip_sweeps; skip-book=$skip_book; skip-rust-class=$skip_rust_class; skip-compile=$skip_compile; skip-wasm=$skip_wasm; skip-fmt=$skip_fmt; selector=$gate_test_selector; nextest=[${nextest_args:-workspace}]${nextest_extra:+ extra=[$nextest_extra]}); log: $log"
    local gate_started; gate_started="$(date +%s)"
    gate_load_start="$(load1)"
    run_gate "$log" "$fuzz_mode" "$gate_scope" "$queue_infra" "$queue_infra_only" \
        "$cargo_target_dir" "$census_proof_sha" "$ungated" "$skip_sweeps" \
        "$skip_book" "$nextest_args" "$nextest_expr" \
        "$skip_rust_class" "$skip_compile" "$skip_wasm" "$check_packages" \
        "$skip_fmt" "$nextest_extra"
    capture_gate_test_metrics "$log"
    if [ "$gate_dtest_status" = "pending" ]; then
        local dtest_values=""
        if [ -s "$gate_dtest_report" ] \
            && dtest_values="$(jq -er '
                select(.schema == 1 and (.tests|type) == "array"
                    and (.partitions|type) == "array"
                    and (.run.target_narrowed|type) == "boolean"
                    and (.run.exact_filter|type) == "boolean"
                    and (.run.filter_bytes|type) == "number"
                    and (.run.required_filter_bytes|type) == "number"
                    and (.run.commands|type) == "number")
                | [.timings.total_ms, .changed_files, .changed_definitions,
                   (.tests|length), .timings.index.elapsed_ms,
                   .timings.index.load_ms, .timings.index.git_ms,
                   .timings.index.stat_ms, .timings.index.read_ms,
                   .timings.index.parse_ms, .timings.index.materialize_ms,
                   .timings.index.graph_ms, .timings.index.serialize_ms,
                   .timings.index.map_ms, .timings.selection.elapsed_ms,
                   .timings.selection.git_diff_ms, .timings.selection.old_sources_ms,
                   .timings.selection.old_parse_us,
                   .timings.selection.definition_compare_us,
                   .timings.selection.old_cache_hits,
                   .timings.selection.old_cache_misses,
                   .timings.selection.old_cache_lookup_us,
                   .timings.selection.old_cache_write_ms,
                   .timings.selection.graph_walk_ms,
                   .timings.selection.materialize_ms, (.partitions|length),
                   .timings.index.overlay_hits, .timings.index.overlay_entries,
                   .timings.index.overlay_compacted,
                   .timings.index.index_overlay_entries,
                   .timings.index.index_overlay_compacted,
                   .run.target_narrowed, .run.exact_filter,
                   .run.filter_bytes, .run.required_filter_bytes, .run.commands,
                   (if .execution.test_identity_source == "junit"
                    then (.execution.executed_tests|length) else "" end),
                   (if .execution.test_identity_source == "junit"
                        and (.execution.selected_not_observed_tests|type) == "number"
                    then .execution.selected_not_observed_tests else "" end)]
                | map(if . == null then "" else tostring end) | join("\u001f")
            ' "$gate_dtest_report" 2>/dev/null)"; then
            IFS=$'\x1f' read -r gate_dtest_elapsed_ms gate_dtest_changed_files \
                gate_dtest_changed_definitions gate_dtest_selected_tests \
                gate_dtest_index_ms gate_dtest_index_load_ms gate_dtest_index_git_ms \
                gate_dtest_index_stat_ms gate_dtest_index_read_ms \
                gate_dtest_index_parse_ms gate_dtest_index_materialize_ms \
                gate_dtest_index_graph_ms gate_dtest_index_serialize_ms \
                gate_dtest_index_map_ms gate_dtest_selection_ms \
                gate_dtest_selection_git_diff_ms gate_dtest_selection_old_sources_ms \
                gate_dtest_selection_old_parse_us gate_dtest_selection_definition_compare_us \
                gate_dtest_selection_old_cache_hits gate_dtest_selection_old_cache_misses \
                gate_dtest_selection_old_cache_lookup_us gate_dtest_selection_old_cache_write_ms \
                gate_dtest_selection_graph_walk_ms gate_dtest_selection_materialize_ms \
                gate_dtest_partitions gate_dtest_overlay_hits \
                gate_dtest_overlay_entries gate_dtest_overlay_compacted \
                gate_dtest_index_overlay_entries gate_dtest_index_overlay_compacted \
                gate_dtest_target_narrowed gate_dtest_exact_filter \
                gate_dtest_filter_bytes gate_dtest_required_filter_bytes \
                gate_dtest_commands gate_dtest_executed_tests \
                gate_dtest_selected_not_observed_tests <<<"$dtest_values"
            local -a dtest_report_files=("$gate_dtest_report")
            local dtest_partition_report="${gate_dtest_report%.json}.partition.json"
            [ ! -s "$dtest_partition_report" ] \
                || dtest_report_files+=("$dtest_partition_report")
            local dtest_summary_values=""
            if dtest_summary_values="$(jq -sr '
                map(select(.schema == 1 and (.tests|type) == "array")) as $reports
                | [($reports|map(.tests|length)|add // 0),
                   ($reports|map(select(.execution.test_identity_source == "junit")
                                  | .execution.executed_tests|length)|add // null),
                   ($reports|map(select(.execution.test_identity_source == "junit")
                                  | .execution.selected_not_observed_tests)|add // null)]
                | map(if . == null then "" else tostring end) | @tsv
            ' "${dtest_report_files[@]}" 2>/dev/null)"; then
                IFS=$'\t' read -r gate_dtest_selected_tests \
                    gate_dtest_executed_tests gate_dtest_selected_not_observed_tests \
                    <<<"$dtest_summary_values"
            fi
            if [ "$gate_dtest_selected_tests" -gt 0 ]; then
                gate_dtest_status="selected"
            else
                gate_dtest_status="empty"
                gate_dtest_reason="no affected tests; used legacy selection"
                gate_test_selector="legacy"
            fi
        else
            gate_dtest_status="fallback"
            gate_dtest_reason="dtest selection failed; used legacy selection"
            gate_test_selector="legacy"
        fi
    fi
    local gate_finished; gate_finished="$(date +%s)"
    gate_load_end="$(load1)"
    local gate_took=$((gate_finished - gate_started))

    case "$gate_result" in
        green) ;;
        red | timeout:*)
            # Red candidates cannot land. Release immediately so diagnosis and
            # journal finalization do not delay the next heavyweight user.
            release_lock
            # A stale incremental link failure is the cache's fault: purge the
            # workspace crates' incremental state and re-gate the same commit
            # once. The marker makes it once per SHA, so a genuine link error
            # still goes red on the second attempt.
            local link_marker="$qdir/link-retried/$sha"
            if [ "$gate_result" = "red" ] && [ ! -e "$link_marker" ] \
                && stale_incremental_link_failure "$log"; then
                mkdir -p "$qdir/link-retried" && : >"$link_marker"
                purge_workspace_incremental "$gate_wt/$cargo_target_dir"
                note "stale incremental link state in $cargo_target_dir; purged it and requeueing $branch once; log: $log"
                local link_requeued; link_requeued="$(date +%s)"
                record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
                    "$lock_acquired" "$gate_started" "$gate_finished" "$link_requeued" sha "$sha" log "$log" \
                    change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
                    reason "stale incremental link state purged" stages "$(stage_summary "$log")"
                local li; for li in "${!batch_ids[@]}"; do
                    set_change_state "${batch_ids[$li]}" queued "${batch_submitted_shas[$li]}" \
                        "${batch_attempt_ids[$li]}" || true
                done
                return 1
            fi
            local why="red" extra=""
            [ "$gate_result" != "red" ] && { why="timeout"; extra="${gate_result#timeout: }"; }
            # So the next attempt for the responsible change does not pay full
            # test execution to rediscover the same failures. A batch head is
            # only the scheduling representative, so do not attach its failure
            # to that change until attribution below identifies the owner.
            local failed_preflight failed_preflight_nextest
            failed_preflight="$(failing_test_expr "$log" || true)"
            failed_preflight_nextest="$(failing_test_nextest "$log" || true)"
            if [ "${#batch_branches[@]}" -gt 1 ]; then
                # A red batch indicts no one member. Keep every queue file;
                # ordered dependency stacks bisect by prefix, while unrelated
                # changes evict a clear culprit (see evict_candidate_index) or
                # fall back to individual re-gates.
                [ "$why" = "red" ] && extra="$(failure_summary "$log")"
                local evict_pick="" evict_index="" evict_score="" split_strategy="individual"
                if [ "$stack_mode" -eq 1 ]; then
                    split_strategy="prefix_split"
                elif [ "$why" = "red" ]; then
                    evict_pick="$(evict_candidate_index "$log" "$base" || true)"
                    evict_index="${evict_pick%% *}"
                    evict_score="${evict_pick##* }"
                    case "$evict_index" in '' | *[!0-9]*) evict_index="" ;; esac
                    [ -z "$evict_index" ] || split_strategy="culprit_evict"
                fi
                note "batch of ${#batch_branches[@]} is $(echo "$why" | tr a-z A-Z) after ${gate_took}s — $extra"
                if [ "$stack_mode" -eq 1 ]; then
                    note "  splitting dependency stack at a validated prefix boundary; log: $log"
                elif [ -n "$evict_index" ]; then
                    note "  evicting likely culprit '${batch_branches[$evict_index]}' (score $evict_score) to an individual gate; the remaining $(( ${#batch_branches[@]} - 1 )) re-gate as one batch; log: $log"
                else
                    note "  re-queueing unrelated members for individual gates; log: $log"
                fi
                local batch_red_finished; batch_red_finished="$(date +%s)"
                record_attempt batch_red "$branch" "$attempt_start" "$prepare_finished" \
                    "$lock_acquired" "$gate_started" "$gate_finished" "$batch_red_finished" \
                    change_id "$change_id" members "${batch_branches[*]}" log "$log" reason "$extra" \
                    strategy "$split_strategy" \
                    stages "$(stage_summary "$log")"
                local bf bi
                for bi in "${!batch_ids[@]}"; do
                    set_change_state "${batch_ids[$bi]}" queued "${batch_submitted_shas[$bi]}" \
                        "${batch_attempt_ids[$bi]}" || true
                done
                if [ "$stack_mode" -eq 1 ]; then
                    # A dependency stack is one ordered candidate. Preserve
                    # the existing replay on its representative while the
                    # prefix splitter finds the failing boundary.
                    remember_preflight "$change_id" "$failed_preflight" "$failed_preflight_nextest"
                    # A dependency stack has an ordered failure boundary. Gate
                    # the first half as one prefix next; green lands that prefix
                    # and exposes the remaining suffix, while another red halves
                    # again. This finds/lands the safe prefix in logarithmic
                    # splits instead of re-gating every member independently.
                    local next_prefix=$(( (${#batch_branches[@]} + 1) / 2 ))
                    set_queue_batch_limit "$f" "$change_id" "$submitted_sha" "$attempt_id" "$next_prefix" || true
                    note "  dependency stack will re-gate prefix of $next_prefix before the blocked suffix"
                elif [ -n "$evict_index" ]; then
                    # The failure-to-diff match gives this replay a concrete
                    # owner. Innocent members re-gate without compiling or
                    # executing a product preflight learned from another diff.
                    remember_preflight "${batch_ids[$evict_index]}" "$failed_preflight" "$failed_preflight_nextest"
                    # Only the evicted member gates alone; everyone else keeps
                    # batching eligibility and re-gates together next loop.
                    mark_queue_entry "${batch_files[$evict_index]}" "${batch_ids[$evict_index]}" \
                        "${batch_submitted_shas[$evict_index]}" "${batch_attempt_ids[$evict_index]}" nobatch || true
                    record evicted "${batch_branches[$evict_index]}" \
                        change_id "${batch_ids[$evict_index]}" \
                        attempt_id "${batch_attempt_ids[$evict_index]}" \
                        submitted_sha "${batch_submitted_shas[$evict_index]}" \
                        score "$evict_score" log "$log" \
                        reason "diff overlaps failing target: $extra"
                else
                    # No clear culprit: an unrelated red batch has no ordered
                    # prefix to trust, so every member re-gates individually.
                    for bi in "${!batch_files[@]}"; do
                        mark_queue_entry "${batch_files[$bi]}" "${batch_ids[$bi]}" \
                            "${batch_submitted_shas[$bi]}" "${batch_attempt_ids[$bi]}" nobatch || true
                    done
                fi
                # Retaining the queue files is right; retaining the HEAD slot is
                # not. Do this last so the marker moves with the renamed entry.
                for bi in "${!batch_files[@]}"; do
                    demote_front_entry "${batch_files[$bi]}" || true
                done
                return 1
            fi
            remember_preflight "$change_id" "$failed_preflight" "$failed_preflight_nextest"
            [ "$why" = "red" ] && extra="$(failure_summary "$log")"
            note "$branch is $(echo "$why" | tr a-z A-Z) after ${gate_took}s — $extra"
            note "  log: $log"
            local failed_finished; failed_finished="$(date +%s)"
            record_attempt "$why" "$branch" "$attempt_start" "$prepare_finished" \
                "$lock_acquired" "$gate_started" "$gate_finished" "$failed_finished" sha "$sha" \
                change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
                log "$log" reason "$extra" stages "$(stage_summary "$log")"
            set_change_state "$change_id" "$why" "$submitted_sha" "$attempt_id" || true
            consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id" && return 0
            note "$branch was resubmitted during its failed gate; preserving the updated queue entry"
            return 1
            ;;
    esac

    # Submission remains concurrent with the gate itself. A branch can be
    # amended after the pre-gate identity check while its old tip is green.
    # Re-check before ANY landing work so the validated but superseded tip
    # cannot reach master (or be journaled as merged) ahead of its replacement.
    local post_gate_stale=0
    for qi in "${!batch_files[@]}"; do
        queue_entry_matches "${batch_files[$qi]}" "${batch_ids[$qi]}" \
            "${batch_submitted_shas[$qi]}" "${batch_attempt_ids[$qi]}" \
            || post_gate_stale=1
    done
    if [ "$post_gate_stale" -eq 1 ]; then
        note "$branch or a batched member was resubmitted during its green gate; refusing to land the superseded candidate"
        local post_gate_requeued; post_gate_requeued="$(date +%s)"
        record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$gate_started" "$gate_finished" "$post_gate_requeued" \
            change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
            sha "$sha" log "$log" reason "submission changed during gate" \
            stages "$(stage_summary "$log")"
        for qi in "${!batch_ids[@]}"; do
            set_change_state "${batch_ids[$qi]}" queued "${batch_submitted_shas[$qi]}" \
                "${batch_attempt_ids[$qi]}" || true
        done
        release_lock
        return 1
    fi

    # TEST-MODE SAFETY: an isolated state dir isolates the queue/journal but
    # NOT the merge target — without this guard a harness test would ff the
    # REAL master (it did, once). Tests must opt in explicitly to merging.
    if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ] && [ "${MERGE_QUEUE_ALLOW_MERGE:-}" != "1" ]; then
        note "test state dir active and MERGE_QUEUE_ALLOW_MERGE!=1 — gate was GREEN but skipping the real merge"
        local validated_finished; validated_finished="$(date +%s)"
        local vi
        for vi in "${!batch_branches[@]}"; do
            record_attempt validated "${batch_branches[$vi]}" "$attempt_start" "$prepare_finished" \
                "$lock_acquired" "$gate_started" "$gate_finished" "$validated_finished" sha "$sha" log "$log" \
                change_id "${batch_ids[$vi]}" attempt_id "${batch_attempt_ids[$vi]}" \
                submitted_sha "${batch_submitted_shas[$vi]}" reason "test mode: merge skipped" \
                stages "$(stage_summary "$log")"
            set_change_state "${batch_ids[$vi]}" validated "${batch_submitted_shas[$vi]}" \
                "${batch_attempt_ids[$vi]}" || true
            consume_queue_entry "${batch_files[$vi]}" "${batch_ids[$vi]}" \
                "${batch_submitted_shas[$vi]}" "${batch_attempt_ids[$vi]}" || true
        done
        release_lock
        return 0
    fi

    if [ "$(git -C "$root" rev-parse master)" != "$base" ]; then
        local replayed=""
        if replayed="$(try_replay_over_docs_only_master "$base" "$sha")"; then
            note "master moved by docs-safe paths only; replaying green $branch onto $(git -C "$root" rev-parse --short master) without re-gating"
            sha="$replayed"
            base="$(git -C "$root" rev-parse master)"
        else
            note "master moved during the gate; requeueing $branch for a fresh rebase"
            local requeued_finished; requeued_finished="$(date +%s)"
            record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
                "$lock_acquired" "$gate_started" "$gate_finished" "$requeued_finished" sha "$sha" log "$log" \
                change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
                reason "master moved" stages "$(stage_summary "$log")"
            local ri; for ri in "${!batch_ids[@]}"; do
                set_change_state "${batch_ids[$ri]}" queued "${batch_submitted_shas[$ri]}" \
                    "${batch_attempt_ids[$ri]}" || true
            done
            release_lock
            return 1 # keep the queue file; the loop will re-process it
        fi
    fi

    local current_branch
    current_branch="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
    if [ "$current_branch" = "master" ]; then
        if ! git -C "$root" merge --ff-only "$sha" >/dev/null 2>&1; then
            local ff_replayed=""
            if ff_replayed="$(try_replay_over_docs_only_master "$base" "$sha")" \
                && sha="$ff_replayed" \
                && git -C "$root" merge --ff-only "$sha" >/dev/null 2>&1; then
                note "fast-forward succeeded after replaying over a docs-only master move"
            else
            # Don't requeue: that would re-run the whole gate for a problem that is
            # in the MAIN worktree (dirty files colliding with the update). The
            # sha is already validated — surface it for a manual ff after cleanup.
            note "fast-forward of master to $sha FAILED (dirty collision in main worktree)."
            note "the gate was GREEN — merge manually with: git merge --ff-only $sha"
            local blocked_finished; blocked_finished="$(date +%s)"
            record_attempt blocked "$branch" "$attempt_start" "$prepare_finished" \
                "$lock_acquired" "$gate_started" "$gate_finished" "$blocked_finished" sha "$sha" log "$log" \
                change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
                reason "ff-merge failed in main worktree" \
                stages "$(stage_summary "$log")"
            set_change_state "$change_id" blocked "$submitted_sha" "$attempt_id" || true
            local bqi; for bqi in "${!batch_ids[@]}"; do
                [ "$bqi" -eq 0 ] || set_change_state "${batch_ids[$bqi]}" queued \
                    "${batch_submitted_shas[$bqi]}" "${batch_attempt_ids[$bqi]}" || true
            done
            consume_queue_entry "$f" "$change_id" "$submitted_sha" "$attempt_id" || true
            release_lock
            return 0
            fi
        fi
    else
        # The main worktree is allowed to be on an agent branch. Do not merge the
        # validated commit into that branch and then journal a false master merge;
        # move only refs/heads/master, guarded by the base SHA that was gated.
        if ! git -C "$root" update-ref refs/heads/master "$sha" "$base" >/dev/null 2>&1; then
            local ref_replayed=""
            if ref_replayed="$(try_replay_over_docs_only_master "$base" "$sha")"; then
                sha="$ref_replayed"
                base="$(git -C "$root" rev-parse master)"
            fi
            if [ -z "$ref_replayed" ] \
                || ! git -C "$root" update-ref refs/heads/master "$sha" "$base" >/dev/null 2>&1; then
            note "fast-forward of refs/heads/master to $sha FAILED (master moved or ref lock failed)."
            local ref_requeued_finished; ref_requeued_finished="$(date +%s)"
            record_attempt requeued "$branch" "$attempt_start" "$prepare_finished" \
                "$lock_acquired" "$gate_started" "$gate_finished" "$ref_requeued_finished" sha "$sha" log "$log" \
                change_id "$change_id" attempt_id "$attempt_id" submitted_sha "$submitted_sha" \
                reason "master ref update failed" stages "$(stage_summary "$log")"
            local rui; for rui in "${!batch_ids[@]}"; do
                set_change_state "${batch_ids[$rui]}" queued "${batch_submitted_shas[$rui]}" \
                    "${batch_attempt_ids[$rui]}" || true
            done
            release_lock
            return 1
            fi
        fi
    fi
    local landed_finished; landed_finished="$(date +%s)"
    note "MERGED ${batch_branches[*]} → master @ $sha (gate ${gate_took}s, ${#batch_branches[@]} branch(es))"
    local i bf
    for i in "${!batch_branches[@]}"; do
        record_attempt merged "${batch_branches[$i]}" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$gate_started" "$gate_finished" "$landed_finished" sha "$sha" log "$log" \
            change_id "${batch_ids[$i]}" attempt_id "${batch_attempt_ids[$i]}" \
            submitted_sha "${batch_submitted_shas[$i]}" batch "${#batch_branches[@]}" \
            stages "$(stage_summary "$log")" \
            flaky_count "$(flaky_summary "$log")" flaky "$(flaky_tests "$log")"
        set_change_state "${batch_ids[$i]}" merged "${batch_submitted_shas[$i]}" \
            "${batch_attempt_ids[$i]}" || true
        bf="${batch_files[$i]}"
        consume_queue_entry "$bf" "${batch_ids[$i]}" "${batch_submitted_shas[$i]}" \
            "${batch_attempt_ids[$i]}" || true
    done
    release_lock
    # Reclaim the merged branches' worktrees (their multi-GB target/) right away.
    # (The branch refs themselves are NOT force-moved: under batching the merged
    # sha contains OTHER branches' commits, and pointing an agent's branch at it
    # would hand the agent unrelated work. sweep deletes fully-merged refs.)
    # A large shared checkout can have hundreds of linked worktrees. Do not hold
    # ready queue items behind that full scan; coalesce reclamation until the
    # queue drains, as already done for patch-equivalent submissions above.
    if [ -n "$(next_ready_queue_file || true)" ]; then
        deferred_sweep=1
    else
        cmd_sweep || true
    fi
    return 0
}

# Idle prewarm: with the queue empty, move the gate worktree to current master
# and rebuild + warm the embedded-program caches so the NEXT gate starts hot
# (saves the incremental rebuild + first-spawn compiles it would otherwise pay
# inside its own wall-clock). Runs under the gate lock so an ad-hoc with-lock
# run can't collide; skipped instantly if anything is queued or already warm.
prewarm_gate() {
    [ -d "$gate_wt" ] || return 0
    ls "$queue_dir"/*.json >/dev/null 2>&1 && return 0
    local m active_target inactive_target incomplete_target="" reset_inactive=0
    m="$(git -C "$root" rev-parse master)"
    [ -f "$qdir/prewarmed" ] && [ "$(cat "$qdir/prewarmed")" = "$m" ] && return 0
    acquire_lock "prewarm: master @ ${m:0:9}"
    # Re-check under the lock — a submit may have raced us.
    if ls "$queue_dir"/*.json >/dev/null 2>&1; then release_lock; return 0; fi
    note "idle: prewarming gate worktree at master ${m:0:9}"
    git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
    git -C "$gate_wt" checkout --detach --quiet "$m" 2>/dev/null || { release_lock; return 0; }
    active_target="$(gate_target_generation)"
    inactive_target="$(inactive_gate_target_generation "$active_target")" \
        || { release_lock; return 0; }
    if [ -f "$prewarm_incomplete" ]; then
        IFS=' ' read -r incomplete_target _ <"$prewarm_incomplete" || incomplete_target=""
        case "$incomplete_target" in
            target | target-prewarm) ;;
            *)
                note "idle: ignoring invalid prewarm-incomplete generation '$incomplete_target'"
                incomplete_target=""
                ;;
        esac
        [ "$incomplete_target" != "$inactive_target" ] || reset_inactive=1
    fi
    # This marker deliberately survives cancellation, failure, or coordinator
    # death. gate-target still names the untouched active generation, while a
    # later idle attempt can rebuild the marked inactive generation in place.
    if ! printf '%s %s\n' "$inactive_target" "$m" >"$prewarm_incomplete"; then
        note "idle: cannot mark $inactive_target prewarm incomplete"
        release_lock
        return 0
    fi
    # Warm every profile the gate uses in the inactive generation: dev + test
    # in $inactive_target. Nothing copies or renames Cargo outputs between
    # generations. Every Cargo phase must succeed before the
    # selector can be promoted; a prewarm failure remains opportunistic and
    # leaves the current active generation unchanged.
    # Prewarm is opportunistic. A submission arriving after the under-lock
    # recheck must preempt it instead of waiting behind a cold multi-profile
    # build. Put the complete tree, including rustup setup that can wait on its
    # global lock, in its own process group before watching the queue. Terminate
    # only the prewarm process group we started.
    # Match run_gate / check.sh once for every Cargo phase below (incremental
    # on, CARGO_PROFILE_TEST_STRIP=symbols, no sccache). A profile mismatch
    # here makes promotion a cold rebuild for the next gate.
    # Cargo can trust a fingerprint whose executable was lost when an old
    # prewarm was cancelled mid-write. Recover only the validated inactive
    # generation, inside this cancellable process group; the active generation
    # and all of its derived directories remain untouched.
    # Lower priority at the process-group root so every descendant inherits it.
    # macOS utility QoS is the measured fix; nice is a conservative portable
    # fallback, and env preserves today's behavior on a minimal host.
    local prewarm_runner=(env)
    if command -v taskpolicy >/dev/null 2>&1; then
        prewarm_runner=(taskpolicy -c utility)
    elif command -v nice >/dev/null 2>&1; then
        prewarm_runner=(nice -n 10)
    fi
    # The coordinator is itself detached, so Bash job control is not
    # sufficient to create a distinct process group for this background job.
    # Without a real session boundary, `$!` is not its PGID: the queue-arrival
    # monitor concludes the group is already gone, then blocks in `wait` while
    # an idle prewarm keeps compiling.  Use the same portable session boundary
    # as `daemon` so cancellation can target only the prewarm tree.
    local prewarm_session=(env)
    # Prefer the in-process Perl binding: command-line `setsid` variants may
    # fork before exec, making `$!` name the short-lived launcher rather than
    # the session/PGID recorded in gate.lock.  Perl replaces this background
    # child after `POSIX::setsid`, so the recorded PID is the cancellable
    # session leader on both macOS and Linux.
    if command -v perl >/dev/null 2>&1; then
        prewarm_session=(perl -MPOSIX -e '
            defined POSIX::setsid() or die "setsid: $!\\n";
            exec @ARGV;
            die "exec: $!\\n";
        ')
    elif command -v setsid >/dev/null 2>&1; then
        prewarm_session=(setsid)
    fi
    ( exec "${prewarm_session[@]}" "${prewarm_runner[@]}" bash -c '
        set -euo pipefail
        gate_wt="$1"
        reset_inactive="$2"
        inactive_target="$3"
        active_target="$4"
        target_cloner="$5"
        cd "$gate_wt"
        if [ "$reset_inactive" -eq 1 ]; then
            rm -rf -- "$inactive_target"
        fi
        # A reclaimed or cancelled inactive generation used to restart from
        # empty directories here. That cold path recompiles Wasmtime and
        # AWS-LC before it reaches any Witchy crate. Seed every gate profile
        # from the validated active generation first; the filtered CoW clone
        # drops path-bound incremental state and loose rcgu objects while
        # retaining dependency fingerprints, build-script outputs and rlibs.
        source_dir="$active_target"
        dest_dir="$inactive_target"
        if [ ! -d "$dest_dir" ]; then
            if [ -d "$source_dir" ]; then
                "$target_cloner" --clone-target "$source_dir" "$dest_dir"
            else
                mkdir -p "$dest_dir"
            fi
        fi
        unset CARGO_INCREMENTAL
        [ ! -f scripts/feature-unification.sh ] || . scripts/feature-unification.sh
        export CARGO_TARGET_DIR="$inactive_target" CARGO_PROFILE_TEST_STRIP=symbols RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER=
        cargo build --workspace >/dev/null 2>&1
        cargo test --workspace --no-run >/dev/null 2>&1
        [ ! -x scripts/warm-witchy-caches.sh ] || ./scripts/warm-witchy-caches.sh >/dev/null 2>&1
    ' prewarm "$gate_wt" "$reset_inactive" "$inactive_target" "$active_target" \
        "$here/scripts/worktree-warm.sh" ) &
    local prewarm_pid=$!
    # `setsid` runs in the new background child, so there is a small window
    # where `$!` still belongs to the coordinator's process group.  Do not
    # probe that not-yet-created group: a false negative would skip the queue
    # monitor and block in `wait` until an idle prewarm finishes.  Wait until
    # the recorded PID is its own process-group leader before publishing it.
    local prewarm_group="" prewarm_group_attempt
    for prewarm_group_attempt in {1..100}; do
        prewarm_group="$(ps -o pgid= -p "$prewarm_pid" 2>/dev/null | tr -d '[:space:]')"
        [ "$prewarm_group" = "$prewarm_pid" ] && break
        pid_is_alive "$prewarm_pid" || break
        sleep 0.01
    done
    if [ "$prewarm_group" != "$prewarm_pid" ]; then
        note "idle: prewarm session leader $prewarm_pid did not become ready"
        wait "$prewarm_pid" || true
        release_lock
        return 0
    fi
    active_gate_pgid="$prewarm_pid"
    printf '%s\n' "$prewarm_pid" >"$lock/gate_pgid"
    local cancelled=0
    while process_group_is_alive "$prewarm_pid"; do
        if ls "$queue_dir"/*.json >/dev/null 2>&1; then
            cancelled=1
            note "queue work arrived; cancelling idle prewarm (pgid $prewarm_pid)"
            # Utility-QoS descendants can need more than the default one-second
            # cleanup grace to run their TERM handlers under machine pressure.
            terminate_gate_process_group "$prewarm_pid" "queued work arrived" 3
            break
        fi
        sleep 1
    done
    local prewarm_status=0
    wait "$prewarm_pid" || prewarm_status=$?
    active_gate_pgid=""
    rm -f "$lock/gate_pgid" 2>/dev/null || true
    if [ "$cancelled" -eq 0 ] && [ "$prewarm_status" -eq 0 ]; then
        # At this point the inactive generation is complete. Clear its marker
        # before promotion: a crash in between merely causes a later rebuild,
        # while gate-target still points at the old validated generation.
        rm -f "$prewarm_incomplete"
        if promote_gate_target "$inactive_target"; then
            if printf '%s\n' "$m" >"$qdir/prewarmed"; then
                note "idle: promoted $inactive_target for master ${m:0:9}"
            else
                note "idle: promoted $inactive_target but could not record prewarmed sha"
            fi
        else
            note "idle: $inactive_target is warm but gate-target promotion failed"
        fi
    fi
    release_lock
}

# Master canary (see canary_enabled above). Runs the serialized gate's own
# workspace test shape -- the shape a Cargo.toml-only batch gets: scope `all`,
# no WITCHY_GATE_NEXTEST selection -- on the exact master tip, in the gate
# worktree, under gate.lock, with fmt/rust-class/compile legs skipped. It never
# merges, never moves master, and yields: ready queue work arriving mid-run
# kills only the canary's own process group. The canaried SHA is recorded only
# for a finished run, so a yielded canary retries the same tip once the queue
# is idle again and the interval has elapsed.
master_canary() {
    [ "$canary_enabled" = 1 ] || return 0
    [ -d "$gate_wt" ] || return 0
    # Never wait for the lock here: a waiting canary would block the loop and
    # delay the next gate. Try again on a later idle pass.
    [ ! -d "$lock" ] || return 0
    queue_has_ready_change && return 0
    local m; m="$(git -C "$root" rev-parse --verify --quiet master)" || return 0
    [ "$(jq -r '.sha // empty' "$canary_state" 2>/dev/null || true)" != "$m" ] || return 0
    local t last_start
    t="$(date +%s)"
    last_start="$(cat "$canary_last_start" 2>/dev/null || true)"
    case "$last_start" in '' | *[!0-9]*) last_start=0 ;; esac
    [ "$((t - last_start))" -ge "$canary_interval" ] || return 0

    local log; log="$logs/$(date +%Y%m%d-%H%M%S)-master-canary-${m:0:12}.log"
    acquire_lock "master canary: ${m:0:9}" "" "$log"
    if queue_has_ready_change || [ "$(git -C "$root" rev-parse master)" != "$m" ]; then
        release_lock
        return 0
    fi
    printf '%s\n' "$t" >"$canary_last_start"
    git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
    if ! git -C "$gate_wt" checkout --detach --quiet "$m" 2>/dev/null; then
        note "idle: master canary could not check out ${m:0:9} in $gate_wt"
        release_lock
        return 0
    fi
    note "idle: master canary (workspace tests) at ${m:0:9}; log: $log"
    record master_canary_started "" sha "$m" log "$log"
    local cargo_target_dir; cargo_target_dir="$(gate_target_generation)"
    gate_preflight_expr=""
    gate_preflight_nextest=""
    gate_yield_probe=queue_has_ready_change
    # fuzz=none scope=all queue-infra=0/0 census-proof="" sweeps on, book
    # skipped, workspace nextest (empty selection), skip rust-class/compile/
    # wasm, no check packages, skip fmt.
    run_gate "$log" none all 0 0 "$cargo_target_dir" "" "glamour grimoire" 0 1 \
        "" "" 1 1 1 "" 1 ""
    gate_yield_probe=""
    local elapsed=$(($(date +%s) - t))
    release_lock

    local result
    case "$gate_result" in
        yielded)
            note "idle: master canary at ${m:0:9} yielded to ready queue work"
            record master_canary_yielded "" sha "$m" log "$log" elapsed_s "$elapsed"
            return 0
            ;;
        green) result=green ;;
        red) result=red ;;
        *) result=timeout ;;
    esac
    local report_field=""
    [ "$result" != red ] || report_field="$canary_report"
    jq -n --arg sha "$m" --arg result "$result" --arg at "$(now)" --arg log "$log" \
        --arg elapsed "$elapsed" --arg report "$report_field" \
        '{sha: $sha, result: $result, at: $at, log: $log, elapsed_s: $elapsed}
         + (if $report == "" then {} else {report: $report} end)' \
        >"$canary_state.tmp" && mv "$canary_state.tmp" "$canary_state"

    case "$result" in
        green)
            printf '%s\n' "$m" >"$canary_last_green"
            rm -f "$canary_report"
            note "idle: master canary GREEN at ${m:0:9}"
            record master_canary_green "" sha "$m" log "$log" elapsed_s "$elapsed"
            ;;
        timeout)
            note "idle: master canary at ${m:0:9} did not finish: ${gate_result#timeout: }"
            record master_canary_timeout "" sha "$m" log "$log" elapsed_s "$elapsed" \
                reason "${gate_result#timeout: }"
            ;;
        red)
            local summary failing last_green range="" range_note
            summary="$(failure_summary "$log")"
            failing="$(strip_ansi <"$log" 2>/dev/null \
                | grep -aE '^[[:space:]]*(TRY [0-9]+ )?(FAIL|TIMEOUT|TMT) \[' \
                | sed 's/^[[:space:]]*//' | sort -u | sed -n '1,40p' || true)"
            last_green="$(cat "$canary_last_green" 2>/dev/null || true)"
            if [ -n "$last_green" ] \
                && git -C "$root" merge-base --is-ancestor "$last_green" "$m" 2>/dev/null; then
                range="$last_green..$m"
                range_note="$(git -C "$root" log --oneline --no-decorate "$range" 2>/dev/null \
                    | sed -n '1,200p')"
            else
                range_note="(no earlier green canary on this history; bisect from the last known-good master)"
            fi
            mkdir -p "$(dirname "$canary_report")"
            {
                printf '# Master canary RED\n\n'
                printf 'Report only: this does not block submissions or gates.\n\n'
                printf -- '- master: `%s`\n' "$m"
                printf -- '- detected: %s\n' "$(now)"
                printf -- '- log: `%s`\n' "$log"
                printf -- '- first failure: %s\n' "$summary"
                printf -- '- last green canary: `%s`\n\n' "${last_green:-none}"
                printf '## Failing tests\n\n```\n%s\n```\n\n' \
                    "${failing:-(no nextest FAIL line; see the log)}"
                printf '## Commits since the last green canary%s\n\n' "${range:+ ($range)}"
                printf '```\n%s\n```\n' "$range_note"
            } >"$canary_report.tmp" && mv "$canary_report.tmp" "$canary_report"
            note "idle: master canary RED at ${m:0:9}: $summary (report: $canary_report)"
            record master_canary_red "" sha "$m" log "$log" elapsed_s "$elapsed" \
                failing "$summary" \
                failing_count "$(printf '%s' "$failing" | grep -c . || true)" \
                last_green "$last_green" range "$range" report "$canary_report"
            ;;
    esac
    return 0
}

next_ready_queue_file() {
    local qf readiness submitted_sha
    while IFS= read -r qf; do
        # Dependency state is irrelevant when the submission has no work left:
        # integration tips often make whole obsolete chains patch-equivalent to
        # master while their historical parents remain red or conflicted.
        submitted_sha="$(jq -r '.sha // empty' "$qf")"
        if submission_is_represented "$submitted_sha"; then
            basename "$qf"
            return 0
        fi
        readiness="$(queue_readiness "$qf")"
        [ "$readiness" = ready ] || continue
        basename "$qf"
        return 0
    done < <(find "$queue_dir" -maxdepth 1 -name '*.json' -print | sort)
    return 1
}

# Cheap admission probe for cleanup. Unlike next_ready_queue_file, this does
# not perform patch-equivalence checks: sweep only needs to yield to an entry
# whose dependency state already says it can run.
queue_has_ready_change() {
    local qf identity branch change_id submitted_sha attempt_id
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        identity="$(jq -r '[.branch // "", .change_id // "", .sha // "", .attempt_id // ""] | @tsv' "$qf" 2>/dev/null || true)"
        IFS=$'\t' read -r branch change_id submitted_sha attempt_id <<<"$identity"
        [ -n "$branch" ] && [ -n "$change_id" ] \
            && [ -n "$submitted_sha" ] && [ -n "$attempt_id" ] || continue
        queue_entry_matches "$qf" "$change_id" "$submitted_sha" "$attempt_id" || continue
        [ "$(queue_readiness "$qf")" = ready ] && return 0
    done
    return 1
}

flush_deferred_sweep() {
    [ "$deferred_sweep" -eq 1 ] || return 0
    cmd_sweep || true
    deferred_sweep=0
}

cmd_run() {
    local once=0
    [ "${1:-}" = "--once" ] && once=1
    loaded_coordinator_hash="$(git -C "$root" hash-object "$coordinator_script" 2>/dev/null || true)"
    if [ -f "$qdir/migrating" ]; then
        note "state migration is in progress; refusing to start a coordinator"
        exit 1
    fi
    # Only the PERSISTENT loop owns coordinator.pid. An ad-hoc `run --once`
    # used to clobber it, then exit — leaving a dead pid in the file, so
    # doctor/submit reported NO COORDINATOR while the real daemon was alive,
    # and the natural reaction (start another daemon) produced TWO coordinators
    # racing the queue. --once also refuses to run alongside a live daemon.
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    if [ -n "$cpid" ] && [ "$cpid" != "$$" ]; then
        if [ "$once" -eq 1 ]; then
            note "a persistent coordinator (pid $cpid) is already running — it will drain the queue; not starting a --once run"
            exit 0
        fi
        note "a coordinator is already running (pid $cpid); refusing to start a second"
        exit 1
    fi
    if [ "$once" -eq 0 ] && adopt_reexec_coordinator_lock; then
        note "adopted coordinator singleton across re-exec (pid $$)"
    elif ! acquire_coordinator_lock; then
        cpid="$(cat "$coordinator_lock/pid" 2>/dev/null || true)"
        if [ "$once" -eq 1 ]; then
            note "a coordinator (pid ${cpid:-?}) won the startup race — it will drain the queue; not starting a --once run"
            exit 0
        fi
        note "a coordinator (pid ${cpid:-?}) won the startup race; refusing to start a second"
        exit 1
    fi
    # Reap before preparation so an abandoned compile/test tree cannot compete
    # with the replacement coordinator's rebase and focused checks.
    reap_stale_gate_lock || true
    recover_orphaned_change_claims
    if [ "$once" -eq 0 ]; then
        echo "$$" >"$qdir/coordinator.pid"
        reap_orphan_coordinators "$$"
        local cppid; cppid="$(ps -o ppid= -p "$$" 2>/dev/null | tr -d ' ' || true)"
        if [ -n "$cppid" ] && [ "$cppid" != 1 ]; then
            note "WARNING: persistent 'run' is session-bound (ppid $cppid); use './scripts/merge-queue.sh daemon' for a durable coordinator"
        fi
    fi
    note "coordinator up (pid $$, gate: '$gate_cmd', timeouts: $(gate_timeout_display) total / ${stall_timeout}s stall / ${workspace_test_budget}s test); state: $qdir"
    if [ -n "${MERGE_QUEUE_DAEMON_READY_FD:-}" ]; then
        case "$MERGE_QUEUE_DAEMON_READY_FD" in
            *[!0-9]*) note "invalid daemon readiness descriptor"; return 1 ;;
        esac
        local ready_fd="$MERGE_QUEUE_DAEMON_READY_FD"
        printf 'ready\n' >&"$ready_fd"
        # macOS ships Bash 3.2, before `exec {var}>&-` dynamic descriptors.
        # The value was constrained to digits above before this small eval.
        eval "exec ${ready_fd}>&-"
        unset MERGE_QUEUE_DAEMON_READY_FD
    fi
    # Disk: reclaim worktrees this queue landed but never swept.
    #
    # The sweep is otherwise OPPORTUNISTIC -- it runs after a merge -- so any
    # worktree whose coordinator died before sweeping, or whose merge predates
    # this coordinator, is orphaned forever. Nothing ever catches up. Measured
    # 2026-08-27: .claude/worktrees had grown to 518 GB on a volume that was
    # 88% full, and 18 landed+clean worktrees had been sitting for 2-9 days. A
    # full disk fails builds in ways that look exactly like a flaky gate, so
    # this is a gate-reliability fix as much as a housekeeping one.
    #
    # cmd_sweep is already conservative -- journal-confirmed merged, clean tree,
    # nothing beyond master by patch id, and `git worktree remove` without
    # --force so git refuses anything surprising -- so running it on a timer
    # adds no new risk, only coverage.
    # Do not make a live queue wait behind a potentially large historical
    # cleanup.  An empty queue still gets the catch-up sweep (and therefore
    # preserves the startup-sweep contract); queued work defers cleanup to the
    # normal post-merge/timer path.
    local startup_queue=0 startup_qf
    for startup_qf in "$queue_dir"/*.json; do
        if [ -f "$startup_qf" ]; then startup_queue=1; break; fi
    done
    if [ "$startup_queue" -eq 0 ]; then
        cmd_sweep || true
    else
        note "startup sweep deferred while queue has pending changes"
    fi
    local sweep_interval="${MERGE_QUEUE_SWEEP_INTERVAL:-1800}"
    local last_sweep; last_sweep="$(date +%s)"

    # Consecutive dirty-checkout deferrals, driving the backoff below.
    local deferrals=0
    # RFC-0159 F. Coordinator uptime is the largest single latency contributor
    # and it is currently invisible, because NOTHING IS WRITTEN WHEN NOTHING IS
    # HAPPENING -- uptime has to be inferred from gaps between merges, which
    # cannot distinguish "no coordinator" from "no queue". A heartbeat makes it
    # directly computable.
    local heartbeat_interval="${MERGE_QUEUE_HEARTBEAT_INTERVAL:-300}"
    local last_heartbeat=0
    while :; do
        local sweep_now; sweep_now="$(date +%s)"
        if [ "$((sweep_now - last_sweep))" -ge "$sweep_interval" ]; then
            last_sweep="$sweep_now"
            cmd_sweep || true
        fi
        local heartbeat_now; heartbeat_now="$(date +%s)"
        if [ "$((heartbeat_now - last_heartbeat))" -ge "$heartbeat_interval" ]; then
            last_heartbeat="$heartbeat_now"
            disk_guard
            record heartbeat "" pid "$$" \
                queued "$(find "$queue_dir" -maxdepth 1 -name '*.json' -print 2>/dev/null | grep -c . || echo 0)"
        fi
        if ! coordinator_lock_owned; then
            note "lost coordinator singleton ownership; exiting instead of becoming an unnamed sibling"
            return 1
        fi
        local f first
        # Drain the sorted listing under pipefail; `head` can SIGPIPE `sort`
        # when a busy queue has enough entries to fill the pipe.
        first="$(find "$queue_dir" -maxdepth 1 -name '*.json' -print | sort | sed -n '1p')"
        if [ -z "$first" ]; then
            flush_deferred_sweep
            if [ "$once" -eq 1 ]; then note "queue drained"; break; fi
            prewarm_gate
            master_canary || true
            sleep "$poll_interval"
            continue
        fi
        f="$(next_ready_queue_file || true)"
        if [ -z "$f" ]; then
            flush_deferred_sweep
            if [ "$once" -eq 1 ]; then
                note "queue has no ready changes (dependencies are waiting or blocked)"
                break
            fi
            master_canary || true
            sleep "$poll_interval"
            continue
        fi
        local process_status=0
        process_one "$queue_dir/$f" || process_status=$?
        reexec_if_coordinator_updated "$once"
        [ "$process_status" -ne 2 ] && deferrals=0
        [ "$process_status" -eq 0 ] && continue
        if [ "$process_status" -eq 2 ]; then
            if [ "$once" -eq 1 ]; then
                note "queue deferred: main master checkout has tracked changes"
                break
            fi
            # Exponential backoff while the blocking reason persists. A flat 15s
            # retry against a checkout nobody is watching produced 1,052 requeues
            # over 24.3h with zero merges repo-wide; the policy (never gate
            # against a dirty shared checkout) is right, only the retry shape was
            # wrong. Log once per backoff period rather than once per poll.
            deferrals=$((deferrals + 1))
            if [ "$deferrals" -eq 3 ]; then
                local blocked_reason; blocked_reason="$(main_worktree_block_reason)"
                note "STALLED: $blocked_reason"
                record stalled "" reason "${blocked_reason:-main master checkout has tracked changes}" \
                    deferrals "$deferrals"
            fi
            local backoff="$poll_interval"
            local doublings=$((deferrals - 1))
            [ "$doublings" -gt 6 ] && doublings=6
            local i=0
            while [ "$i" -lt "$doublings" ]; do
                backoff=$((backoff * 2))
                i=$((i + 1))
            done
            [ "$backoff" -gt 600 ] && backoff=600
            sleep "$backoff"
        else
            sleep "$retry_interval"
        fi
    done
}

# Remove worktrees whose branch this queue has MERGED or found already
# represented (per journal.jsonl) and whose tree is clean + fully contained in
# master. A successful journal event is the load-bearing guard: a FRESH agent
# worktree (branch at master, no commits yet) is indistinguishable from a merged
# one by ahead-count alone — sweeping on that heuristic would delete a working
# agent's checkout. Never touches the main worktree or the gate worktree. Each
# removal frees a multi-GB target/.
cmd_sweep() {
    if [ ! -f "$journal" ]; then note "sweep: no journal; nothing merged yet"; return 0; fi
    # With ready work pending, defer the full walk but still recycle RFC-0171
    # pool slots: at most WITCHY_WORKTREE_POOL_MAX of them, each a cheap
    # detach, and a busy queue is exactly when agents are claiming slots.
    local pool_only=0
    if queue_has_ready_change; then
        deferred_sweep=1
        declare -F worktree_pool_release >/dev/null || {
            note "sweep: ready queue work is pending; deferring cleanup"
            return 0
        }
        note "sweep: ready queue work is pending; deferring cleanup except pool slots"
        pool_only=1
    fi
    # The journal is append-only and this singleton coordinator cannot append
    # another merge while it is sweeping. Parse the merged branch set once.
    # Re-reading the full journal for every linked worktree made reclamation
    # quadratic in repository history (13,642 rows x 450 worktrees in the
    # measured 2026-09-12 queue). Queue entries remain a live per-worktree
    # check below because submit may race this function.
    local merged_branches
    merged_branches="$(jq -r 'select(.event=="merged" or .event=="already_merged") | .branch' "$journal" | sort -u)"
    # The exact tips that landed ("<branch> <submitted sha>"). A worktree still
    # AT its landed submission added nothing after it, even when the gate's
    # rebase resolved a conflict and `git cherry` no longer sees the patch.
    local merged_tips
    merged_tips="$(jq -r 'select(.event=="merged" and .submitted_sha != null) | "\(.branch) \(.submitted_sha)"' "$journal" | sort -u)"
    local swept=0 sweep_probe=0
    while IFS= read -r wt; do
        if [ "$pool_only" -eq 1 ]; then
            case "$wt" in */.claude/worktrees/pool-[0-9][0-9]) ;; *) continue ;; esac
        fi
        sweep_probe=$((sweep_probe + 1))
        if [ "$pool_only" -eq 0 ] && [ "$((sweep_probe % 16))" -eq 0 ] && queue_has_ready_change; then
            deferred_sweep=1
            note "sweep: ready queue work arrived; deferring remaining cleanup after $swept worktree(s)"
            return 0
        fi
        [ "$wt" = "$root" ] && continue
        [ "$wt" = "$gate_wt" ] && continue
        [ -d "$wt" ] || continue
        local branch; branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
        case "$branch" in '?' | HEAD) continue ;; esac
        # The guard: only branches this queue landed or proved represented, and
        # not re-queued since.
        grep -Fqx -- "$branch" <<<"$merged_branches" || continue
        if ls "$queue_dir"/*.json >/dev/null 2>&1 && \
           jq -r .branch "$queue_dir"/*.json | grep -Fqx -- "$branch"; then continue; fi
        # Safety: clean and nothing beyond master. Gated merges land a REBASED
        # sha, so the original commits are often not ancestors — `git cherry`
        # treats patch-equivalent commits (all "-") as merged too.
        [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || continue
        if [ "$(git -C "$wt" rev-list --count "master..HEAD" 2>/dev/null || echo 1)" != "0" ] \
            && ! grep -Fqx -- "$branch $(git -C "$wt" rev-parse HEAD 2>/dev/null)" <<<"$merged_tips"; then
            git -C "$wt" cherry master HEAD 2>/dev/null | grep -q '^+' && continue
        fi
        # RFC-0171: a pool worktree is recycled in place (detached at master,
        # marked free) so its path-keyed incremental state serves the next
        # task. Anything else, or a recycle that fails, is removed as before.
        if declare -F worktree_pool_release >/dev/null \
            && worktree_pool_release "$root" "$wt"; then
            note "sweep: recycled merged+clean pool worktree $wt (branch $branch)"
            git -C "$root" branch -d "$branch" >/dev/null 2>&1 || true
            record swept "$branch" worktree "$wt" recycled 1
            swept=$((swept + 1))
            continue
        fi
        note "sweep: removing merged+clean worktree $wt (branch $branch)"
        if git -C "$root" worktree remove "$wt" >/dev/null 2>&1; then
            git -C "$root" branch -d "$branch" >/dev/null 2>&1 || true
            record swept "$branch" worktree "$wt"
            swept=$((swept + 1))
        else
            note "sweep: git worktree remove refused $wt; leaving it"
        fi
    done < <(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2}')
    [ "$pool_only" -eq 0 ] || return 0
    if queue_has_ready_change; then
        deferred_sweep=1
        note "sweep: ready queue work arrived; deferring historical branch cleanup"
        return 0
    fi
    # Deterministic concurrency seam for the fixture that submits work after
    # branch pruning has begun. Production never sets this variable.
    if [ -n "${MERGE_QUEUE_TEST_SWEEP_BRANCH_MARKER:-}" ]; then
        : >"$MERGE_QUEUE_TEST_SWEEP_BRANCH_MARKER"
        local marker_wait=0
        while [ ! -f "$MERGE_QUEUE_TEST_SWEEP_BRANCH_MARKER.continue" ] \
            && [ "$marker_wait" -lt 500 ]; do
            sleep 0.01
            marker_wait=$((marker_wait + 1))
        done
    fi
    # Journal-merged branches with no worktree left behind are safe to drop too.
    # Intersect the two sorted sets once instead of starting `git show-ref` for
    # every historical merge. The measured journal named 2,464 merged branch
    # rows, while most refs had already been deleted.
    #
    # A rebased landing leaves the original submitted ref outside master's
    # ancestry, so `branch -d` correctly refuses it. Once this same ref has
    # already passed the worktree sweep, retrying that unchanged refusal after
    # every unrelated merge is pure overhead. Derive the latest branch
    # lifecycle from the existing journal and skip only an exact submitted tip
    # that was superseded by a different landed SHA and then swept. A newer
    # submission resets the lifecycle; an advanced or unmatched ref misses the
    # exact branch+OID intersection and keeps the normal safe deletion check.
    local local_branch_refs processed_superseded processed_superseded_count
    local_branch_refs="$(git -C "$root" for-each-ref \
        --format='%(refname:lstrip=2)%09%(objectname)' refs/heads | sort -u)"
    processed_superseded="$(comm -12 \
        <(jq -sr '
            reduce .[] as $e ({};
                ($e.branch // "") as $branch |
                if $branch == "" then .
                elif $e.event == "submitted" then del(.[$branch])
                elif (($e.event == "merged" or $e.event == "already_merged")
                      and ($e.submitted_sha // "") != ""
                      and ($e.sha // "") != "") then
                    .[$branch] = {submitted:$e.submitted_sha, landed:$e.sha, swept:false}
                elif ($e.event == "swept" and .[$branch] != null) then
                    .[$branch].swept = true
                else . end)
            | to_entries[]
            | select(.value.swept and .value.submitted != .value.landed)
            | [.key, .value.submitted] | @tsv
        ' "$journal" | sort -u) \
        <(printf '%s\n' "$local_branch_refs") | cut -f1)"
    processed_superseded_count="$(printf '%s\n' "$processed_superseded" | grep -c . || true)"
    [ "$processed_superseded_count" -eq 0 ] \
        || note "sweep: skipped $processed_superseded_count unchanged superseded branch ref(s) already swept"
    local b branch_batches=0 branch_candidates=0
    local branch_batch=()
    while IFS= read -r b; do
        [ -n "$b" ] || continue
        branch_batch+=("$b")
        branch_candidates=$((branch_candidates + 1))
        [ "${#branch_batch[@]}" -ge 16 ] || continue
        # `git branch -d` validates every named ref independently. Batch the
        # validation so a large landed-ref backlog does not pay one Git process
        # startup per branch, then yield before starting another bounded batch.
        git -C "$root" branch -d -- "${branch_batch[@]}" >/dev/null 2>&1 || true
        branch_batches=$((branch_batches + 1))
        branch_batch=()
        if queue_has_ready_change; then
            deferred_sweep=1
            note "sweep: ready queue work arrived; deferring remaining historical branch cleanup"
            return 0
        fi
    done < <(comm -12 \
        <(printf '%s\n' "$merged_branches") \
        <(printf '%s\n' "$local_branch_refs" | cut -f1) \
        | awk 'NR==FNR { skip[$0]=1; next } !($0 in skip)' \
            <(printf '%s\n' "$processed_superseded") -)
    if [ "${#branch_batch[@]}" -gt 0 ]; then
        git -C "$root" branch -d -- "${branch_batch[@]}" >/dev/null 2>&1 || true
        branch_batches=$((branch_batches + 1))
    fi
    [ "$branch_candidates" -eq 0 ] \
        || note "sweep: checked $branch_candidates historical branch candidate(s) in $branch_batches batch(es)"
    note "sweep: removed $swept worktree(s)"
}

# Block until <branch> reaches a terminal journal event newer than this call
# (merged/already_merged/red/timeout/conflict/blocked/dropped), print that event
# as JSON. Exit 0 iff the branch is merged or was already represented. For
# agents: `submit X && wait X` replaces polling loops.
cmd_wait() {
    local branch="${1:?usage: merge-queue.sh wait <branch> [timeout-secs]}"
    local budget="${2:-3600}"
    local start_line=0
    [ -f "$journal" ] && start_line="$(wc -l <"$journal" | tr -d ' ')"
    local waited=0 ev
    while [ "$waited" -le "$budget" ]; do
        if [ -f "$journal" ]; then
            ev="$(tail -n "+$((start_line + 1))" "$journal" | jq -c --arg b "$branch" \
                'select(.branch==$b) | select(.event=="merged" or .event=="already_merged" or .event=="red" or .event=="timeout" or .event=="conflict" or .event=="blocked" or .event=="dropped")' \
                | tail -1)"
            if [ -n "$ev" ]; then
                echo "$ev"
                case "$(echo "$ev" | jq -r .event)" in
                    merged | already_merged) return 0 ;;
                    *) return 1 ;;
                esac
            fi
        fi
        sleep 10; waited=$((waited + 10))
    done
    note "wait: no terminal event for $branch within ${budget}s"
    return 2
}

# Journal analytics: outcome counts, gate-time trend, suspected-flaky tests
# (failed in one gate, passed in a later one), repeat-red branches.
# RFC-0159 G. Windowed reporting has exactly one implementation, and it is
# `gate-report.sh` -- which already takes --since/--json and buckets by load per
# core. This used to print lifetime-cumulative counts from a second, unwindowed
# code path; two reporters meant two schemas and two definitions of a window,
# which is the drift this RFC exists to prevent. Per house style the cumulative
# implementation is deleted in one cut and the name survives as a forwarder, so
# `merge-queue.sh stats` keeps working without being a second implementation.
cmd_stats() {
    local report="$root/scripts/gate-report.sh"
    if [ ! -x "$report" ]; then
        note "gate-report.sh is missing; nothing to report"
        exit 1
    fi
    local args=("--state-dir" "$qdir")
    if [ "$#" -eq 0 ]; then
        args+=("--since" "7d")
    else
        args+=("$@")
    fi
    exec "$report" "${args[@]}"
}

# Retire a superseded pending submission without deleting or rewriting its
# branch. This is deliberately explicit and audited: shared worktrees often
# still own the ref, while an integration tip on master has made the queued
# attempt obsolete in a way patch-id comparison cannot prove automatically.
cmd_drop() {
    local branch="${1:?usage: merge-queue.sh drop <branch> <reason>}"
    shift
    local reason="$*"
    [ -n "$reason" ] || { note "drop requires an auditable reason"; exit 2; }

    local cf change_id submitted_sha attempt_id state qf="" candidate tmp
    cf="$(change_file_for_branch "$branch")"
    acquire_change_lock
    if [ ! -f "$cf" ]; then
        release_change_lock
        note "no known change for '$branch'"
        exit 2
    fi
    change_id="$(jq -r '.change_id // empty' "$cf")"
    submitted_sha="$(jq -r '.current_sha // empty' "$cf")"
    attempt_id="$(jq -r '.current_attempt // empty' "$cf")"
    state="$(jq -r '.state // empty' "$cf")"
    case "$state" in
        gating | validated)
            release_change_lock
            note "cannot drop '$branch' while its current attempt is $state"
            exit 1
            ;;
    esac
    for candidate in "$queue_dir"/*.json; do
        [ -f "$candidate" ] || continue
        if queue_entry_matches "$candidate" "$change_id" "$submitted_sha" "$attempt_id"; then
            qf="$candidate"
            break
        fi
    done
    if [ -z "$qf" ]; then
        release_change_lock
        note "no pending queue entry for '$branch'"
        exit 2
    fi
    tmp="$cf.tmp.$$"
    jq --arg state dropped --arg updated "$(now)" --arg reason "$reason" \
        '.state=$state | .updated=$updated | .drop_reason=$reason' "$cf" >"$tmp"
    mv "$tmp" "$cf"
    rm -f "$qf" "$qf.nobatch" "$qf.batch-limit"
    release_change_lock

    record dropped "$branch" change_id "$change_id" attempt_id "$attempt_id" \
        submitted_sha "$submitted_sha" reason "$reason" via "operator drop"
    note "dropped pending submission for $branch: $reason"
}

# After a `blocked` event (gate green, ff-merge refused by the main worktree)
# the operator merges manually — which leaves the journal's last word as
# "blocked" and misleads every agent reading it. `resolve` closes the record:
# it verifies the journaled sha actually IS on master, then journals `merged`.
cmd_resolve() {
    local branch="${1:?usage: merge-queue.sh resolve <branch>}"
    local sha submitted_sha attempt_id change_id
    sha="$(jq -r --arg b "$branch" 'select(.event=="blocked" and .branch==$b) | .sha' "$journal" 2>/dev/null | tail -1)"
    [ -n "$sha" ] || { note "no blocked event for '$branch' in the journal"; exit 2; }
    if ! git -C "$root" merge-base --is-ancestor "$sha" master; then
        note "$sha is NOT on master — merge it first: git merge --ff-only $sha"
        exit 1
    fi
    submitted_sha="$(jq -r --arg b "$branch" \
        'select(.event=="blocked" and .branch==$b) | (.submitted_sha // .sha)' \
        "$journal" 2>/dev/null | tail -1)"
    attempt_id="$(jq -r --arg b "$branch" \
        'select(.event=="blocked" and .branch==$b) | (.attempt_id // "")' \
        "$journal" 2>/dev/null | tail -1)"
    change_id="$(change_id_for_branch "$branch" 2>/dev/null || true)"
    record merged "$branch" sha "$sha" change_id "$change_id" via "manual ff after blocked"
    set_change_state "$change_id" merged "$submitted_sha" "$attempt_id" || true
    note "journaled merged for $branch @ $sha"
    cmd_sweep || true
}

cmd_daemon() {
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    if [ -n "$cpid" ]; then
        note "coordinator already running (pid $cpid); nothing to do"
        return 0
    fi
    # `nohup` ignores SIGHUP but does NOT leave the launcher's process group.
    # Terminal/tool supervisors commonly terminate that whole group, which used
    # to orphan an in-flight gate with no coordinator left to merge it. Establish
    # a real session boundary before execing the persistent loop.
    if command -v setsid >/dev/null 2>&1; then
        # util-linux `-f` also works when an interactive shell made the launcher
        # a process-group leader (a group leader cannot call setsid directly).
        nohup setsid -f "$coordinator_script" run \
            >>"$qdir/coordinator.log" 2>&1 </dev/null &
        disown || true
    elif command -v perl >/dev/null 2>&1; then
        # macOS has no setsid(1), but its system Perl exposes POSIX::setsid.
        # Keep the Perl parent in the foreground until the detached child has
        # claimed the singleton and acknowledged readiness over a pipe. Under
        # macOS utility scheduling, a background-only child can otherwise be
        # starved while every concurrent caller waits for coordinator.pid.
        # Fork once so the child cannot be a process-group leader, then replace
        # it with the coordinator. All ordinary descriptors are detached.
        nohup perl -MPOSIX -MFcntl=F_SETFD -e '
            pipe(my $reader, my $writer) or die "pipe: $!\n";
            my $pid = fork();
            defined $pid or die "fork: $!\n";
            if ($pid) {
                close $writer;
                my $ready = <$reader>;
                exit(defined($ready) && $ready eq "ready\n" ? 0 : 1);
            }
            close $reader;
            defined POSIX::setsid() or die "setsid: $!\n";
            my $daemon = fork();
            defined $daemon or die "daemon fork: $!\n";
            exit 0 if $daemon;
            fcntl($writer, F_SETFD, 0) or die "clear close-on-exec: $!\n";
            $ENV{MERGE_QUEUE_DAEMON_READY_FD} = fileno($writer);
            exec @ARGV;
            die "exec: $!\n";
        ' "$coordinator_script" run >>"$qdir/coordinator.log" 2>&1 </dev/null || true
    else
        note "daemon requires setsid(1) or Perl POSIX::setsid to detach safely"
        return 1
    fi
    # Concurrent daemon callers may all fork before the winning child publishes
    # coordinator.pid. Give that child a bounded window instead of reporting a
    # false startup failure after one fixed sleep; the singleton lock still
    # decides which child wins.
    local waited=0
    cpid=""
    while [ "$waited" -lt 5 ]; do
        sleep 1
        cpid="$(coordinator_pid 2>/dev/null || true)"
        [ -n "$cpid" ] && break
        waited=$((waited + 1))
    done
    if [ -n "$cpid" ]; then
        note "coordinator daemon started (pid $cpid); log: $qdir/coordinator.log; stop: kill $cpid"
    else
        note "daemon failed to start — see $qdir/coordinator.log"
        return 1
    fi
}

# One-time production cutover from scratch/merge-queue to state/merge-queue.
# The legacy path becomes a relative symlink, so old agents and absolute paths
# already stored in journal.jsonl keep working. Both singleton locks are held
# across the move; coordinator.pid also names this process so pre-fix daemons
# refuse to start during the compatibility window.
cmd_migrate_state() {
    if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ] || [ -n "${WITCHY_STATE_DIR:-}" ]; then
        note "migrate-state only operates on the repository's default production state"
        return 2
    fi
    local legacy="$root/scratch/merge-queue"
    local state_root="$root/state"
    local target="$state_root/merge-queue"
    local link_target="../state/merge-queue"

    if [ -L "$legacy" ]; then
        if [ -d "$target" ] && [ "$legacy" -ef "$target" ]; then
            note "state already migrated: $target (legacy symlink present)"
            return 0
        fi
        note "refusing: legacy symlink does not resolve to canonical state: $legacy"
        return 1
    fi
    [ "$qdir" = "$legacy" ] || { note "refusing: active state is $qdir, expected legacy $legacy"; return 1; }
    [ -d "$legacy" ] || { note "refusing: legacy state directory is absent: $legacy"; return 1; }
    [ ! -e "$target" ] || { note "refusing: target already exists without a completed legacy symlink: $target"; return 1; }
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    [ -z "$cpid" ] || { note "refusing: coordinator pid $cpid is still running; drain the queue and stop it first"; return 1; }
    if ls "$queue_dir"/*.json >/dev/null 2>&1; then
        note "refusing: queue is not drained"
        return 1
    fi
    [ ! -d "$lock" ] || { note "refusing: gate lock is still held"; return 1; }

    acquire_coordinator_lock || { note "refusing: coordinator singleton is held"; return 1; }
    echo "$$" >"$qdir/coordinator.pid"
    acquire_lock "state migration: scratch/merge-queue -> state/merge-queue"
    acquire_change_lock
    migration_marker_active=1
    : >"$qdir/migrating"
    if ls "$queue_dir"/*.json >/dev/null 2>&1; then
        note "refusing: queue changed while acquiring migration locks"
        rm -f "$qdir/migrating"
        return 1
    fi

    mkdir -p "$state_root"
    local compat_tmp="$root/scratch/.merge-queue-compat-$$"
    if ! ln -s "$link_target" "$compat_tmp"; then
        note "could not prepare the legacy compatibility symlink"
        return 1
    fi
    if ! mv "$legacy" "$target"; then
        rm -f "$compat_tmp"
        note "state move failed; legacy directory remains authoritative"
        return 1
    fi
    # All held locks and the marker moved with the directory. Point cleanup and
    # subsequent journal writes at their new real location before attempting
    # the compatibility symlink, so even a failed symlink install cannot leave
    # an invisible live lock behind in state/.
    qdir="$target"
    queue_dir="$qdir/queue"
    changes_dir="$qdir/changes"
    change_lock="$qdir/change.lock"
    journal="$qdir/journal.jsonl"
    logs="$qdir/logs"
    lock="$qdir/gate.lock"
    coordinator_lock="$qdir/coordinator.lock"
    if ! mv "$compat_tmp" "$legacy"; then
        note "legacy symlink install failed after the move"
        if [ ! -e "$legacy" ] && [ ! -L "$legacy" ]; then
            if mv "$target" "$legacy"; then
                qdir="$legacy"
                queue_dir="$qdir/queue"
                changes_dir="$qdir/changes"
                change_lock="$qdir/change.lock"
                journal="$qdir/journal.jsonl"
                logs="$qdir/logs"
                lock="$qdir/gate.lock"
                coordinator_lock="$qdir/coordinator.lock"
            else
                note "ERROR: rollback failed; state remains at $target"
            fi
        else
            note "ERROR: legacy path was recreated concurrently; canonical state remains at $target"
        fi
        return 1
    fi

    mkdir -p "$state_root/agents"
    if [ ! -f "$state_root/README.txt" ]; then
        printf '%s\n' \
            'Witchy local operational state (gitignored).' \
            'merge-queue/ contains queue, journal, logs, locks, and coordinator data.' \
            'agents/ is available for local agent handoffs and ongoing-work metadata.' \
            >"$state_root/README.txt"
    fi
    release_migration_marker
    record state_migrated "" from "$legacy" to "$target" compatibility_symlink "$legacy"
    release_change_lock
    release_lock
    release_coordinator_lock
    note "migrated operational state to $target"
    note "legacy compatibility: $legacy -> $link_target"
}

cmd_with_lock() {
    [ "${1:-}" = "--" ] && shift
    [ "$#" -ge 1 ] || { note "usage: merge-queue.sh with-lock -- <cmd...>"; exit 2; }
    # Yield to a coordinator that is already waiting. Fail OPEN on every doubt --
    # a dead holder, an unreadable marker, or a wait past the cap all proceed --
    # so this can never wedge with-lock.
    # BUG-632: yield on LIVENESS, never on a per-caller deadline. The cap used to
    # be measured per caller while the intent's lifetime was the coordinator's
    # whole wait, so each arriving caller waited its own 900s, failed open, and
    # took the lock -- leaving the coordinator behind a fresh holder every time.
    # One caller per cap starved it forever (observed 2026-08-27: 100 minutes,
    # zero merges, queue 10 -> 18). Waiting while the coordinator keeps proving
    # it is alive cannot cascade, because acquiring clears the intent.
    local yielded=0 ipid
    while intent_is_live; do
        if [ "$yielded" -eq 0 ]; then
            ipid="$(cat "$lock_intent" 2>/dev/null || true)"
            note "with-lock: yielding to the waiting coordinator (pid $ipid)"
            yielded=1
        fi
        sleep 5
    done
    # Fail OPEN on a dead or unreadable marker, and clear it so it stops costing
    # every future caller a liveness probe. A merely STALE intent is left alone:
    # its owner may still be alive and about to refresh it.
    if [ -f "$lock_intent" ]; then
        ipid="$(cat "$lock_intent" 2>/dev/null || true)"
        case "$ipid" in
            ''|*[!0-9]*) rm -f "$lock_intent" 2>/dev/null || true ;;
            *) kill -0 "$ipid" 2>/dev/null || rm -f "$lock_intent" 2>/dev/null || true ;;
        esac
    fi
    acquire_lock "with-lock: $*"
    local rc=0
    "$@" || rc=$?
    release_lock
    return "$rc"
}

case "${1:-}" in
    submit)    shift; cmd_submit "$@" ;;
    status)    cmd_status ;;
    doctor)    cmd_doctor ;;
    run)       shift; cmd_run "$@" ;;
    daemon)    cmd_daemon ;;
    migrate-state) cmd_migrate_state ;;
    wait)      shift; cmd_wait "$@" ;;
    stats)     shift; cmd_stats "$@" ;;
    drop)      shift; cmd_drop "$@" ;;
    resolve)   shift; cmd_resolve "$@" ;;
    sweep)     shift; cmd_sweep "$@" ;;
    with-lock) shift; cmd_with_lock "$@" ;;
    -h | --help | "") sed -n '2,68p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) note "unknown subcommand '${1}' (try submit, status, doctor, run, daemon, migrate-state, drop, sweep, with-lock)"; exit 2 ;;
esac
