#!/usr/bin/env bash
# Merge/gate coordinator for concurrent agents (see CLAUDE.md "Concurrent
# agents"). It fixes the two expensive failure modes of parallel work:
# duplicated full gates running at once (they stretch each other's long-tail
# e2e tests, and the publish e2e is load-flaky), and a merge landing while a
# full gate runs (which invalidates that gate and forces a rebase + rerun).
#
# The protocol:
#   * Agents work in isolated worktrees and run only FOCUSED tests there
#     (a check.sh shard: --fast / --e2e / --examples / --wasm).
#   * When a branch is ready, the agent runs:  scripts/merge-queue.sh submit <branch>
#   * ONE coordinator session runs:            scripts/merge-queue.sh run
#     It takes branches FIFO, rebases each onto current master in a dedicated
#     warm gate worktree (.claude/worktrees/merge-gate), runs the full gate
#     there under the gate lock, and on green fast-forwards master. If master
#     moved while a gate ran, the candidate is re-rebased and re-gated instead
#     of merging a stale validation. A red/timed-out candidate is journaled and
#     dropped; the queue CONTINUES to the next branch (resubmit after fixing).
#   * Anything else heavyweight (an ad-hoc full suite) should share the same
#     lock:  scripts/merge-queue.sh with-lock -- ./scripts/check.sh --fast
#
# Stall resistance: the gate runs in its own process group under a monitor.
# NEXTEST_STATUS_LEVEL=pass makes nextest stream one line per finished test, so
# a healthy gate writes constantly; if the log goes quiet for
# MERGE_QUEUE_STALL_TIMEOUT seconds (default 300) or the whole gate exceeds
# MERGE_QUEUE_GATE_TIMEOUT seconds (default 2700), the process group is killed,
# the candidate is journaled as timed out, the lock is released, and the queue
# moves on. Logs are always preserved under scratch/merge-queue/logs/.
#
# State is machine-readable and lives under gitignored scratch/merge-queue/
# IN THE MAIN WORKTREE (each worktree has its own scratch/, so state written
# elsewhere would be invisible to the coordinator):
#   queue/*.json    one pending submission per file (FIFO by filename)
#   journal.jsonl   append-only events: submitted/merged/red/timeout/conflict/
#                   requeued/blocked/dropped (red+timeout carry the log path)
#   logs/           full gate output per attempt (check.sh stage markers carry
#                   t+<seconds> offsets, so per-stage timing is in every log)
#   gate.lock/      the lock: pid + what + branch + log + started epoch
#                   (stale locks — dead pid — are stolen)
#
#   scripts/merge-queue.sh submit <branch> [note]   enqueue a local branch
#   scripts/merge-queue.sh status                   queue + in-flight gate + recent journal (JSON)
#   scripts/merge-queue.sh doctor                   human health check: coordinator alive?
#                                                   lock stale? current stage? log fresh?
#   scripts/merge-queue.sh run [--once]             coordinator loop (--once: drain and exit)
#   scripts/merge-queue.sh with-lock -- <cmd...>    run any command under the gate lock
#
# The gate command defaults to `./scripts/check.sh` (the push gate minus e2e);
# override with MERGE_QUEUE_GATE_CMD (e.g. "./scripts/check.sh --full").
set -euo pipefail

# All state lives under the MAIN worktree, no matter which worktree this script
# is invoked from. MERGE_QUEUE_STATE_DIR / MERGE_QUEUE_GATE_WT exist so tests
# can run against throwaway state without touching the live queue.
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | head -1 | sed 's/^worktree //')"
qdir="${MERGE_QUEUE_STATE_DIR:-$root/scratch/merge-queue}"
queue_dir="$qdir/queue"
journal="$qdir/journal.jsonl"
logs="$qdir/logs"
lock="$qdir/gate.lock"
gate_wt="${MERGE_QUEUE_GATE_WT:-$root/.claude/worktrees/merge-gate}"
gate_cmd="${MERGE_QUEUE_GATE_CMD:-./scripts/check.sh}"
gate_timeout="${MERGE_QUEUE_GATE_TIMEOUT:-2700}"
stall_timeout="${MERGE_QUEUE_STALL_TIMEOUT:-300}"

mkdir -p "$queue_dir" "$logs"

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
note() { printf 'merge-queue: %s\n' "$*" >&2; }
strip_ansi() { sed "s/$(printf '\033')\[[0-9;]*m//g"; }

# The last `==> [N] stage (t+Ns)` marker in a gate log = the stage running now.
current_stage() { { strip_ansi <"$1" 2>/dev/null || true; } | { grep -E '^==> \[' || true; } | tail -1 | sed 's/^==> //'; }
# All markers, one line: "[1] build (t+0s);[2] clippy (t+41s);..."
stage_summary() { { strip_ansi <"$1" 2>/dev/null || true; } | { grep -E '^==> \[' || true; } | sed 's/^==> //' | paste -sd';' -; }

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

holding_lock=0
release_lock() { if [ "$holding_lock" -eq 1 ]; then rm -rf "$lock"; fi; holding_lock=0; }
trap release_lock EXIT

acquire_lock() { # acquire_lock <description> [branch] [log]
    local waited=0
    while ! mkdir "$lock" 2>/dev/null; do
        local pid; pid="$(cat "$lock/pid" 2>/dev/null || true)"
        if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
            note "stealing stale gate lock (pid $pid is gone)"
            rm -rf "$lock"
            continue
        fi
        if [ "$waited" -eq 0 ]; then
            note "gate lock held by pid ${pid:-?} ($(cat "$lock/what" 2>/dev/null || echo '?')); waiting"
        fi
        waited=1
        sleep 10
    done
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
        "$root/scripts/worktree-warm.sh" "$gate_wt" 1>&2 || true
    fi
    # Recover from a previous run that died mid-rebase.
    git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
}

# Run the gate in its own process group with a stall/overall-timeout monitor.
# Sets gate_result to "green", "red", or "timeout: <why>". Never returns nonzero.
gate_result=""
run_gate() { # run_gate <log>
    local log="$1"
    local start; start="$(date +%s)"
    # `set -m` puts the background job in its own process group, so a timeout
    # can kill the WHOLE cargo/nextest tree, not just the top shell.
    set -m
    ( cd "$gate_wt" && exec env NEXTEST_STATUS_LEVEL=pass bash -c "$gate_cmd" ) >"$log" 2>&1 &
    local gpid=$!
    set +m
    local why=""
    while :; do
        if ! kill -0 "$gpid" 2>/dev/null; then
            if wait "$gpid"; then gate_result="green"; else gate_result="red"; fi
            return 0
        fi
        sleep 10
        local t; t="$(date +%s)"
        local elapsed=$((t - start))
        local mtime; mtime="$(stat -f %m "$log" 2>/dev/null || echo "$t")"
        local age=$((t - mtime))
        if [ "$elapsed" -gt "$gate_timeout" ]; then
            why="gate exceeded ${gate_timeout}s (MERGE_QUEUE_GATE_TIMEOUT)"
            break
        fi
        if [ "$age" -gt "$stall_timeout" ]; then
            why="no log output for ${age}s (MERGE_QUEUE_STALL_TIMEOUT=${stall_timeout})"
            break
        fi
    done
    note "killing gate (pgid $gpid): $why"
    kill -TERM -- "-$gpid" 2>/dev/null || kill -TERM "$gpid" 2>/dev/null || true
    sleep 5
    kill -KILL -- "-$gpid" 2>/dev/null || true
    wait "$gpid" 2>/dev/null || true
    gate_result="timeout: $why"
    return 0
}

cmd_submit() {
    local branch="${1:?usage: merge-queue.sh submit <branch> [note]}"
    local msg="${2:-}"
    git -C "$root" rev-parse --verify --quiet "refs/heads/$branch" >/dev/null \
        || { note "no local branch '$branch'"; exit 2; }
    local fname; fname="$(date +%s)-$(echo "$branch" | tr '/' '~').json"
    jq -cn --arg branch "$branch" --arg ts "$(now)" \
           --arg sha "$(git -C "$root" rev-parse "refs/heads/$branch")" \
           --arg by "${USER:-unknown}" --arg note "$msg" \
           '{branch: $branch, sha: $sha, submitted: $ts, by: $by, note: $note}' \
        >"$queue_dir/$fname"
    record submitted "$branch" by "${USER:-unknown}"
    note "queued $branch ($fname); a coordinator running 'merge-queue.sh run' will gate + merge it"
}

# Shared by status (JSON) and doctor (prose): what is in flight right now?
inflight_vars() {
    lk_pid="$(cat "$lock/pid" 2>/dev/null || true)"
    lk_what="$(cat "$lock/what" 2>/dev/null || true)"
    lk_branch="$(cat "$lock/branch" 2>/dev/null || true)"
    lk_log="$(cat "$lock/log" 2>/dev/null || true)"
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
    jq -n \
        --slurpfile q <(cat "$queue_dir"/*.json 2>/dev/null || true) \
        --slurpfile j <(tail -20 "$journal" 2>/dev/null || true) \
        --arg pid "$lk_pid" --arg what "$lk_what" --arg branch "$lk_branch" \
        --arg log "$lk_log" --arg stage "$lk_stage" \
        --arg elapsed "$lk_elapsed" --arg log_age "$lk_log_age" \
        '{queue: $q,
          gate_lock: (if $pid == "" then null else
            {pid: $pid, what: $what, branch: $branch, log: $log,
             stage: $stage, elapsed_s: $elapsed, log_age_s: $log_age} end),
          recent: $j}'
}

cmd_doctor() {
    echo "merge-queue doctor — $(now)"
    local cpid; cpid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null; then
        echo "coordinator : RUNNING (pid $cpid)"
    else
        echo "coordinator : NOT RUNNING${cpid:+ (last pid $cpid is dead)} — start: ./scripts/merge-queue.sh run"
    fi
    local n; n="$(ls -1 "$queue_dir" 2>/dev/null | wc -l | tr -d ' ')"
    if [ "$n" -gt 0 ]; then
        echo "queue       : $n pending — $(ls -1 "$queue_dir" | sort | paste -sd' ' -)"
    else
        echo "queue       : empty"
    fi
    if [ -d "$lock" ]; then
        inflight_vars
        local health="ALIVE"
        if [ -z "$lk_pid" ] || ! kill -0 "$lk_pid" 2>/dev/null; then health="STALE (holder dead — next acquire steals it)"; fi
        echo "gate lock   : held by pid ${lk_pid:-?} — $health"
        echo "  what      : ${lk_what:-?}"
        if [ -n "$lk_branch" ]; then echo "  branch    : $lk_branch"; fi
        if [ -n "$lk_elapsed" ]; then echo "  elapsed   : ${lk_elapsed}s (timeout ${gate_timeout}s)"; fi
        if [ -n "$lk_log" ]; then
            echo "  log       : $lk_log"
            echo "  log age   : ${lk_log_age:-?}s since last output (stall kill at ${stall_timeout}s)"
            echo "  stage     : ${lk_stage:-(no stage marker yet)}"
        fi
    else
        echo "gate lock   : free"
    fi
    echo "recent      :"
    if [ -f "$journal" ]; then
        tail -5 "$journal" | jq -r '"  \(.ts)  \(.event)\t\(.branch)\(if .reason then "  (" + .reason + ")" else "" end)"'
    else
        echo "  (no journal yet)"
    fi
}

process_one() { # process_one <queue-file>; returns 0 if the file was consumed
    local f="$1"
    local branch; branch="$(jq -r .branch "$f")"
    if ! git -C "$root" rev-parse --verify --quiet "refs/heads/$branch" >/dev/null; then
        note "branch $branch vanished; dropping"
        record dropped "$branch" reason "branch deleted"
        rm -f "$f"
        return 0
    fi

    # Take the lock BEFORE touching the gate worktree: the checkout/rebase below
    # would corrupt a gate another lock-holder is running there right now.
    local t0; t0="$(date +%s)"
    acquire_lock "full gate: $branch" "$branch" ""
    ensure_gate_worktree
    local base; base="$(git -C "$root" rev-parse master)"

    git -C "$gate_wt" checkout --detach --quiet "refs/heads/$branch"
    if ! git -C "$gate_wt" rebase master >/dev/null 2>&1; then
        git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
        release_lock
        note "$branch does not rebase cleanly onto master — needs a human/agent rebase"
        record conflict "$branch" base "$base"
        rm -f "$f"
        return 0
    fi
    local sha; sha="$(git -C "$gate_wt" rev-parse HEAD)"

    local log; log="$logs/$(date +%Y%m%d-%H%M%S)-$(echo "$branch" | tr '/' '~').log"
    echo "$log" >"$lock/log"
    note "gating $branch (rebased to $sha on $base); log: $log"
    run_gate "$log"
    release_lock
    local took=$(( $(date +%s) - t0 ))

    case "$gate_result" in
        green) ;;
        red)
            note "$branch is RED after ${took}s — see $log"
            record red "$branch" sha "$sha" log "$log" elapsed_s "$took" stages "$(stage_summary "$log")"
            rm -f "$f"
            return 0
            ;;
        timeout:*)
            note "$branch TIMED OUT after ${took}s — ${gate_result#timeout: } — see $log"
            record timeout "$branch" sha "$sha" log "$log" elapsed_s "$took" \
                reason "${gate_result#timeout: }" stages "$(stage_summary "$log")"
            rm -f "$f"
            return 0
            ;;
    esac

    if [ "$(git -C "$root" rev-parse master)" != "$base" ]; then
        note "master moved during the gate; requeueing $branch for a fresh rebase"
        record requeued "$branch" sha "$sha" reason "master moved"
        return 1 # keep the queue file; the loop will re-process it
    fi

    if ! git -C "$root" merge --ff-only "$sha" >/dev/null 2>&1; then
        # Don't requeue: that would re-run the whole gate for a problem that is
        # in the MAIN worktree (not on master, or dirty files colliding with the
        # update). The sha is already validated — surface it for a manual ff.
        note "fast-forward of master to $sha FAILED (main worktree not on master, or dirty collision)."
        note "the gate was GREEN — merge manually with: git merge --ff-only $sha"
        record blocked "$branch" sha "$sha" reason "ff-merge failed in main worktree" log "$log"
        rm -f "$f"
        return 0
    fi
    # Point the submitted branch at its merged (rebased) sha so its worktree
    # sees itself as merged. Fails harmlessly if that branch is checked out.
    git -C "$root" branch -f "$branch" "$sha" >/dev/null 2>&1 || true
    note "MERGED $branch → master @ $sha (gate ${took}s)"
    record merged "$branch" sha "$sha" log "$log" elapsed_s "$took" stages "$(stage_summary "$log")"
    rm -f "$f"
    return 0
}

cmd_run() {
    local once=0
    [ "${1:-}" = "--once" ] && once=1
    echo "$$" >"$qdir/coordinator.pid"
    note "coordinator up (pid $$, gate: '$gate_cmd', timeouts: ${gate_timeout}s total / ${stall_timeout}s stall); state: $qdir"
    while :; do
        local f
        f="$(ls -1 "$queue_dir" 2>/dev/null | sort | head -1 || true)"
        if [ -z "$f" ]; then
            if [ "$once" -eq 1 ]; then note "queue drained"; break; fi
            sleep 15
            continue
        fi
        process_one "$queue_dir/$f" || sleep 5
    done
}

cmd_with_lock() {
    [ "${1:-}" = "--" ] && shift
    [ "$#" -ge 1 ] || { note "usage: merge-queue.sh with-lock -- <cmd...>"; exit 2; }
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
    with-lock) shift; cmd_with_lock "$@" ;;
    -h | --help | "") sed -n '2,56p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) note "unknown subcommand '${1}' (try submit, status, doctor, run, with-lock)"; exit 2 ;;
esac
