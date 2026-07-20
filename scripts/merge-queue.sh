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
# Timed-out candidates are journaled, the lock is released, and the queue moves
# on. Logs are always preserved under state/merge-queue/logs/.
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
#   scripts/merge-queue.sh status                   queue + in-flight gate + recent journal (JSON)
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
# The gate command defaults to `./scripts/check.sh` (the push gate minus e2e);
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
qdir="$(witchy_merge_queue_state_dir "$root")"
queue_dir="$qdir/queue"
changes_dir="$qdir/changes"
change_lock="$qdir/change.lock"
journal="$qdir/journal.jsonl"
logs="$qdir/logs"
lock="$qdir/gate.lock"
coordinator_lock="$qdir/coordinator.lock"
gate_target_file="$qdir/gate-target"
prewarm_incomplete="$qdir/prewarm-incomplete"
gate_wt="${MERGE_QUEUE_GATE_WT:-$root/.claude/worktrees/merge-gate}"
gate_cmd="${MERGE_QUEUE_GATE_CMD:-./scripts/check.sh}"
gate_cmd_is_default=1
[ -z "${MERGE_QUEUE_GATE_CMD+x}" ] || gate_cmd_is_default=0
coordinator_script="${MERGE_QUEUE_COORDINATOR_SCRIPT:-$root/scripts/merge-queue.sh}"
gate_timeout="${MERGE_QUEUE_GATE_TIMEOUT:-0}"
stall_timeout="${MERGE_QUEUE_STALL_TIMEOUT:-600}"
monitor_interval="${MERGE_QUEUE_MONITOR_INTERVAL:-10}"
retry_interval="${MERGE_QUEUE_RETRY_INTERVAL:-5}"
poll_interval="${MERGE_QUEUE_POLL_INTERVAL:-15}"

mkdir -p "$queue_dir" "$changes_dir" "$logs"

# Patch-equivalent submissions are often consumed in long runs after an
# integration tip lands. Coalesce their worktree reclamation into one sweep.
deferred_sweep=0
active_gate_pgid=""

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
note() { printf 'merge-queue: %s\n' "$*" >&2; }
strip_ansi() { sed "s/$(printf '\033')\[[0-9;]*m//g"; }

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
    fail_line="$(echo "$plain" | grep -m1 '^ *FAIL \[' | sed 's/^ *//' || true)"
    [ -z "$fail_line" ] && fail_line="$(echo "$plain" | grep -m1 '^error\[E\|^error:' | head -c120 || true)"
    [ -z "$fail_line" ] && fail_line="$(echo "$plain" | grep -m1 'could not compile' || true)"
    local summary="${stage:+$stage}"
    [ -n "$fail_line" ] && summary="${summary:+$summary: }$fail_line"
    echo "${summary:-unknown failure}"
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

# Record one coordinator attempt with phase boundaries captured by process_one.
# Keep elapsed_s as the actual gate duration for existing journal consumers;
# attempt_elapsed_s covers dequeue through the final validation decision. All
# values are integer wall seconds because macOS /bin/date has no portable
# sub-second epoch format.
record_attempt() { # event branch start prepared locked gate-start gate-end finish [key value]...
    local event="$1" branch="$2" attempt_start="$3" prepare_finished="$4"
    local lock_acquired="$5" gate_start="$6" gate_end="$7" finish="$8"
    shift 8
    record "$event" "$branch" \
        attempt_timing_schema "1" \
        elapsed_s "$((gate_end - gate_start))" \
        attempt_elapsed_s "$((finish - attempt_start))" \
        prepare_elapsed_s "$((prepare_finished - attempt_start))" \
        lock_wait_s "$((lock_acquired - prepare_finished))" \
        preflight_elapsed_s "$((gate_start - lock_acquired))" \
        gate_elapsed_s "$((gate_end - gate_start))" \
        landing_elapsed_s "$((finish - gate_end))" \
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
    for cf in "$changes_dir"/*.json; do
        [ -f "$cf" ] || continue
        jq -er --arg id "$1" 'select(.change_id==$id) | .state' "$cf" 2>/dev/null && return 0
    done
    return 1
}

change_branch_for_id() { # change_branch_for_id <change-id>
    local cf
    for cf in "$changes_dir"/*.json; do
        [ -f "$cf" ] || continue
        jq -er --arg id "$1" 'select(.change_id==$id) | .branch' "$cf" 2>/dev/null && return 0
    done
    return 1
}

set_change_state() { # set_change_state <change-id> <state> [current-sha] [current-attempt]
    local id="$1" state="$2" expected_sha="${3:-}" expected_attempt="${4:-}" cf tmp
    [ -n "$id" ] || return 0
    acquire_change_lock
    for cf in "$changes_dir"/*.json; do
        [ -f "$cf" ] || continue
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
    done
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
    for cf in "$changes_dir"/*.json; do
        [ -f "$cf" ] || continue
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
    done
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
    local cf state id sha attempt branch qf tmp matched
    acquire_change_lock
    for cf in "$changes_dir"/*.json; do
        [ -f "$cf" ] || continue
        state="$(jq -r '.state // empty' "$cf")"
        case "$state" in gating | validated) ;; *) continue ;; esac
        id="$(jq -r '.change_id // empty' "$cf")"
        sha="$(jq -r '.current_sha // empty' "$cf")"
        attempt="$(jq -r '.current_attempt // empty' "$cf")"
        branch="$(jq -r '.branch // empty' "$cf")"
        matched=0
        for qf in "$queue_dir"/*.json; do
            [ -f "$qf" ] || continue
            if queue_entry_matches "$qf" "$id" "$sha" "$attempt"; then
                matched=1
                break
            fi
        done
        [ "$matched" -eq 1 ] || continue
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
    local branch="$1" id="$2" sha="$3" after="$4" state="$5" attempt="$6" cf tmp created
    cf="$(change_file_for_branch "$branch")"
    created="$(jq -r '.created // empty' "$cf" 2>/dev/null || true)"
    [ -n "$created" ] || created="$(now)"
    tmp="$cf.tmp.$$"
    jq -cn --arg branch "$branch" --arg id "$id" --arg sha "$sha" \
        --arg created "$created" --arg updated "$(now)" --arg state "$state" --arg attempt "$attempt" \
        --argjson after "$after" \
        '{schema:1, change_id:$id, branch:$branch, current_sha:$sha,
          current_attempt:$attempt, after:$after, state:$state,
          created:$created, updated:$updated}' >"$tmp"
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
release_lock() { if [ "$holding_lock" -eq 1 ]; then rm -rf "$lock"; fi; holding_lock=0; }
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
        sleep "$monitor_interval"
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

# A checked-out master with tracked edits can make the final fast-forward fail
# after a successful, expensive gate. Untracked local state is intentionally
# excluded: it is common in the shared checkout and only blocks Git when the
# candidate would overwrite the same path.
main_worktree_is_ready_to_land() {
    local current_branch
    current_branch="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
    [ "$current_branch" != "master" ] && return 0
    git -C "$root" diff --quiet --ignore-submodules -- \
        && git -C "$root" diff --cached --quiet --ignore-submodules --
}

# Is the gate's process group actively using CPU? `set -m` puts the gate in its
# own group whose pgid == the launched pid, so `ps -g <pgid>` lists the whole
# cargo/rustc/nextest tree. Sum their %cpu (a float across cores); "busy" means
# the gate is compiling or running tests, not hung — used to distinguish a silent
# WORKING gate from a silent WEDGED one. Fails safe: if ps yields nothing (group
# already gone, or an unexpected ps), returns false so the stall path proceeds.
group_is_busy() { # group_is_busy <pgid>
    local total
    total="$(ps -g "$1" -o %cpu= 2>/dev/null | awk '{s+=$1} END {print s+0}')"
    # Threshold well above idle jitter but far below one busy core (100%); a live
    # compile/test tree sits at hundreds of %.
    awk -v c="$total" 'BEGIN { exit !(c > 20) }'
}

# Run the gate in its own process group with a stall monitor and optional
# emergency whole-gate timeout.
# Sets gate_result to "green", "red", or "timeout: <why>". Never returns nonzero.
gate_result=""
gate_attempt=0
run_gate() { # run_gate <log> [fuzz-mode] [gate-scope] [queue-infra] [queue-infra-only] [cargo-target] [census-proof-sha]
    local log="$1"
    local progress_file="${log}.progress"
    local fuzz_mode="${2:-full}"
    local gate_scope="${3:-all}"
    local queue_infra="${4:-0}"
    local queue_infra_only="${5:-0}"
    local cargo_target_dir="${6:-target}"
    local census_proof_sha="${7:-}"
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
    # keeps daemon and foreground coordinators equivalent. Incremental output
    # remains disabled because this worktree is repeatedly rebased across
    # unrelated branches and otherwise accumulates low-value state.
    set -m
    # Test execution runs at nextest's normal width. The macOS dyld stall that
    # once motivated NEXTEST_TEST_THREADS=4 here was a DISCOVERY problem —
    # CPU-count concurrent cold first-execs of ~100 MB --list binaries — and is
    # solved at the source by scripts/nextest-list-wrapper.sh
    # (.config/nextest.toml); by run time every binary is loader-warm. Bounding
    # execution as well doubled gate wall-clock (measured 2026-07-16: ~20.6 min
    # at width 4 vs the historical 8-10 min).
    rm -f "$progress_file"
    ( cd "$gate_wt" && exec env "CARGO_TARGET_DIR=$cargo_target_dir" CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= NEXTEST_STATUS_LEVEL=pass "WITCHY_GATE_PROGRESS_FILE=$progress_file" "WITCHY_GATE_FUZZ=$fuzz_mode" "WITCHY_GATE_SCOPE=$gate_scope" "WITCHY_GATE_QUEUE_INFRA=$queue_infra" "WITCHY_GATE_CENSUS_PROOF_SHA=$census_proof_sha" bash -c "$selected_gate_cmd" ) >"$log" 2>&1 &
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
    while :; do
        if ! pid_is_alive "$gpid"; then
            if wait "$gpid"; then gate_result="green"; else gate_result="red"; fi
            active_gate_pgid=""
            rm -f "$lock/gate_pgid" 2>/dev/null || true
            rm -f "$progress_file"
            return 0
        fi
        sleep "$monitor_interval"
        local t; t="$(date +%s)"
        local elapsed=$((t - start))
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
    sleep 5
    kill -KILL -- "-$gpid" 2>/dev/null || true
    wait "$gpid" 2>/dev/null || true
    active_gate_pgid=""
    rm -f "$lock/gate_pgid" 2>/dev/null || true
    rm -f "$progress_file"
    gate_result="timeout: $why"
    return 0
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
            submitted_sha "$sha" after "$after_words" by "${USER:-unknown}"
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
            submitted_sha "$sha" after "$after_words" by "${USER:-unknown}"
        note "queued $branch as $change_id ($fname)"
    fi
    release_change_lock
    local cpid; cpid="$(coordinator_pid 2>/dev/null || true)"
    if [ -n "$cpid" ]; then
        note "coordinator (pid $cpid) will gate + merge it"
    else
        note "NO COORDINATOR RUNNING — your submission will sit until one starts:"
        note "  ./scripts/merge-queue.sh daemon"
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
    jq -n \
        --argjson q "$queue_json" \
        --slurpfile j <(tail -20 "$journal" 2>/dev/null || true) \
        --arg pid "$lk_pid" --arg gate_pgid "$lk_gate_pgid" \
        --arg what "$lk_what" --arg branch "$lk_branch" \
        --arg log "$lk_log" --arg stage "$lk_stage" \
        --arg elapsed "$lk_elapsed" --arg log_age "$lk_log_age" \
        '{queue: $q,
          gate_lock: (if $pid == "" then null else
            {pid: $pid, gate_pgid: $gate_pgid, what: $what, branch: $branch, log: $log,
             stage: $stage, elapsed_s: $elapsed, log_age_s: $log_age} end),
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
        echo "queue       : $n pending — ${queue_files[*]}"
    else
        echo "queue       : empty"
    fi
    if [ -d "$lock" ]; then
        inflight_vars
        local health="ALIVE"
        if [ -z "$lk_pid" ] || ! pid_is_alive "$lk_pid"; then health="STALE (holder dead — next acquire steals it)"; fi
        echo "gate lock   : held by pid ${lk_pid:-?} — $health"
        echo "  what      : ${lk_what:-?}"
        if [ -n "$lk_gate_pgid" ]; then echo "  gate pgid : $lk_gate_pgid"; fi
        if [ -n "$lk_branch" ]; then echo "  branch    : $lk_branch"; fi
        if [ -n "$lk_elapsed" ]; then echo "  elapsed   : ${lk_elapsed}s (whole-gate timeout $(gate_timeout_display))"; fi
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
    cid="$(jq -r '.change_id // empty' "$qf")"
    [ -n "$cid" ] || return 1
    list_contains "$cid" "${batch_ids[@]}" && return 1
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

# Deterministic generated snapshots (the RFC-0087 census TSV and the
# `witchy doc`-rendered spec/stdlib.md) go stale whenever an unrelated branch
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
#   * regen is skipped when the batch diff stays inside the docs-safe set
#     (same set as the gate-scope classifier) and does not touch the census
#     snapshot: a docs-only gate must stay seconds, not pay a build. For code
#     diffs the `cargo build` here is not wasted work — nextest needs the same
#     dev-profile `witchy` bin artifacts inside the gate. Keep Cargo's
#     incremental setting identical to run_gate as well; changing that flag in
#     one shared target invalidates the preparation artifacts and forces the
#     full gate to rebuild every workspace crate.
rebaseline_generated_snapshots() { # rebaseline_generated_snapshots <base> <branch> <change-id> <attempt-id> <cargo-target>
    local rb_base="$1" rb_branch="$2" rb_change_id="$3" rb_attempt_id="$4"
    local rb_target="${5:-target}"
    local pre_diff unsafe tmp census_tmp census_err files="" census_run_ok=0 census_stderr_clean=0
    census_proof_ready=0
    pre_diff="$(git -C "$gate_wt" diff --name-only --no-renames "$rb_base..HEAD" 2>/dev/null || true)"
    [ -n "$pre_diff" ] || return 0
    unsafe="$(printf '%s\n' "$pre_diff" | grep -vE '^(rfcs/|wiki/|bugs/|external-refs/|scratch/|security-eval/)' || true)"
    if [ -z "$unsafe" ] \
        && ! printf '%s\n' "$pre_diff" | grep -cx 'rfcs/0087-migration-census\.tsv' >/dev/null; then
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
    if ! ( cd "$gate_wt" && env "CARGO_TARGET_DIR=$rb_target" CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= \
        cargo build --bin witchy --bin rfc0087-census ) >/dev/null 2>&1; then
        note "rebaseline: generator build failed; skipping regen (the gate adjudicates)"
        return 0
    fi
    tmp="$(mktemp "${TMPDIR:-/tmp}/witchy-rebaseline-XXXXXX")"
    census_tmp="$(mktemp "${TMPDIR:-/tmp}/witchy-census-proof-XXXXXX")"
    census_err="$(mktemp "${TMPDIR:-/tmp}/witchy-census-proof-error-XXXXXX")"
    # Each generator writes to a temp file first: a mid-run failure must never
    # truncate the committed snapshot, and an identical output must not dirty
    # the tree.
    if [ -f "$gate_wt/rfcs/0087-migration-census.tsv" ] \
        && ( cd "$gate_wt" && "./$rb_target/debug/rfc0087-census" . ) >"$census_tmp" 2>"$census_err"; then
        census_run_ok=1
        [ ! -s "$census_err" ] && census_stderr_clean=1
        if ! cmp -s "$census_tmp" "$gate_wt/rfcs/0087-migration-census.tsv"; then
            cat "$census_tmp" >"$gate_wt/rfcs/0087-migration-census.tsv"
            files="rfcs/0087-migration-census.tsv"
        fi
    fi
    if [ -f "$gate_wt/spec/stdlib.md" ] && ls "$gate_wt"/std/*.witchy >/dev/null 2>&1 \
        && ( cd "$gate_wt" && "./$rb_target/debug/witchy" doc std/*.witchy ) >"$tmp" 2>/dev/null \
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
        # `FAIL [   1.234s] crate[::binary] test::path`
        # shellcheck disable=SC2046 — word splitting is the parse here.
        set -- $(printf '%s\n' "$fail_line" | sed -E 's/^[[:space:]]*(TRY [0-9]+ )?[A-Z]+ \[[^]]*\][[:space:]]*//')
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

    local best=0 best_index="" tie=0 mi f nf stem score
    for mi in "${!batch_submitted_shas[@]}"; do
        score=0
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
                    [ -n "$crate" ] && case "$f" in src/* | tests/*) score=$((score + 1)) ;; esac
                    ;;
                *)
                    case "$f" in "crates/$crate/"*) score=$((score + 2)) ;; esac
                    ;;
            esac
        done < <(git -C "$root" diff --name-only --no-renames "$ev_base...${batch_submitted_shas[$mi]}" 2>/dev/null || true)
        if [ "$score" -gt "$best" ]; then
            best="$score"; best_index="$mi"; tie=0
        elif [ "$score" -eq "$best" ] && [ "$best" -gt 0 ]; then
            tie=1
        fi
    done
    [ -n "$best_index" ] && [ "$best" -gt 0 ] && [ "$tie" -eq 0 ] || return 0
    printf '%s %s\n' "$best_index" "$best"
}

process_one() { # process_one <queue-file>; returns 0 if the file was consumed
    local f="$1"
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

    # Build from the current master tree and replay only the submitted patches.
    # `process_one` is called from an `||` list, which disables Bash's implicit
    # errexit inside the function, so guard the correctness boundary explicitly:
    # a sandbox-denied index lock must never leave a stale checkout to be gated.
    if ! git -C "$gate_wt" checkout --detach --quiet "$base"; then
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
    local batch_ceiling="$batch_max" batch_limit docs_batch_mode=0 initial_diff=""
    case "$docs_batch_max" in '' | *[!0-9]* | 0) docs_batch_max="$batch_max" ;; esac
    # Raise only the documentation ceiling. The classifier is intentionally
    # stricter than gate_scope=docs: spec/book/README Markdown still receives
    # the full product gate, but may be integrated in one large compatible
    # batch. Any code/config/non-Markdown path keeps the semantic ceiling.
    initial_diff="$(git -C "$gate_wt" diff --name-only --no-renames "$base..HEAD" 2>/dev/null || true)"
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

    local sha; sha="$(git -C "$gate_wt" rev-parse HEAD)"

    # Fuzz policy from the diff (see check.sh's WITCHY_GATE_FUZZ). The differential
    # fuzzer is a fixed-seed parity REGRESSION suite, so it can only catch a bug in
    # a change that could alter backend behavior. Classify the whole batch's diff
    # (base..sha covers every batched branch): if nothing under the parity surface
    # changed, skip it; if the surface changed, run a reduced 10-seed sample (the
    # full 30 still run post-merge on CI under the checked heap, and in `--full`).
    # Fail SAFE: any doubt (git error, empty diff) -> full.
    #
    # Parity surface (reduced): compiler crates, std library, src/, examples,
    # build infrastructure (Cargo.toml/lock, .cargo/, build.rs, rust-toolchain).
    # Non-parity (skip): rfcs/, bugs/, docs, scripts/, projects/ (witchy apps
    # that exercise the compiler but can't change its behavior), book/.
    # --no-renames: with rename detection, `--name-only` reports only the
    # POST-image path — a `git mv std/foo.witchy rfcs/bar.md` would show up as
    # nothing but an rfcs/ file and classify as a docs-only diff while
    # deleting code from master. Listing both sides makes the delete visible.
    # (`grep -c … >/dev/null` instead of `grep -q`: -q exits at the first
    # match, and under `set -o pipefail` a >64KiB $changed can then SIGPIPE
    # the echo — status 141 — misclassifying a matching diff as non-matching.
    # -c always reads all input.)
    local fuzz_mode="full"
    local changed
    if changed="$(git -C "$gate_wt" diff --name-only --no-renames "$base..$sha" 2>/dev/null)" && [ -n "$changed" ]; then
        if echo "$changed" | grep -cE '^(crates/|std/|src/|examples/|build\.rs|Cargo\.(toml|lock)|\.cargo/|rust-toolchain)' >/dev/null; then
            fuzz_mode="reduced"
        else
            fuzz_mode="skip"
        fi
    fi

    # Gate scope from the same diff (see check.sh's WITCHY_GATE_SCOPE): if EVERY
    # changed path is documentation that no test or gate stage reads, the heavy
    # stages could only re-validate master's already-gated tree — skip them and
    # let post-merge CI's complete run be the backstop. The safe set is
    # deliberately TINY: rfcs/ (except rfcs/performance-modes.md, which
    # example_tests::public_sources_do_not_call_legacy_render_intrinsic reads —
    # it panics if that path vanishes), wiki/ and bugs/ (tracked, but read by
    # no test or gate stage — if a test ever starts reading them, REMOVE them
    # from this list), external-refs/ (vendored research notes, never build
    # inputs), and scratch//security-eval/ (gitignored). Everything
    # else — book/, spec/, README.md, std/, scripts/, .claude/, .github/,
    # Cargo metadata — runs the full gate. Fail SAFE: errored/empty diff ->
    # all. The --no-renames above matters doubly here: without it a rename
    # INTO the safe set would hide the deletion side entirely.
    # (Capture-and-test rather than `grep -qv`: an inverted quiet match is the
    # one grep construct with divergent exit semantics across implementations,
    # e.g. a ugrep-shimmed PATH — and this decision gates merges.)
    local gate_scope="all"
    local unsafe_paths=""
    if [ -n "$changed" ]; then
        unsafe_paths="$(echo "$changed" | grep -vE '^(rfcs/|wiki/|bugs/|external-refs/|scratch/|security-eval/)' || true)"
    fi

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
    if [ -n "$changed" ] && [ -z "$unsafe_paths" ] \
        && ! echo "$changed" | grep -cx 'rfcs/performance-modes\.md' >/dev/null; then
        gate_scope="docs"
    fi

    # Queue fixtures manipulate process groups, detached daemons, file locks,
    # and nested Git repositories. Run that binary in check.sh's isolated,
    # serial shard only when this batch can change the queue substrate. The
    # ordinary product suite always excludes it, avoiding load-induced false
    # reds without weakening validation of relevant infrastructure changes.
    local queue_infra=0
    if [ -n "$changed" ] \
        && printf '%s\n' "$changed" \
            | grep -cE '^(\.config/nextest\.toml|scripts/(check|gate-report|merge-queue|nextest-list-wrapper|state-paths|test-for-paths|worktree-status|worktree-warm)\.sh|tests/(merge_queue|test_for_paths)\.rs)$' >/dev/null; then
        queue_infra=1
    fi

    # Queue-core changes are validated by their process-isolated fixture shard,
    # not by re-running an unrelated product tree already green on master. Keep
    # this allowlist intentionally narrower than queue_infra above: check.sh,
    # nextest configuration, reporting, and worktree tooling can affect general
    # product-gate behavior and therefore retain the complete gate. Documentation
    # may ride with a queue-core fix because it is not executable input.
    local queue_infra_only=0
    local non_queue_core=""
    if [ "$queue_infra" -eq 1 ] && [ -n "$changed" ]; then
        non_queue_core="$(printf '%s\n' "$changed" \
            | grep -vE '^(scripts/(merge-queue|state-paths)\.sh|scripts/MERGE-QUEUE\.md|tests/merge_queue\.rs|rfcs/|wiki/|bugs/|external-refs/|scratch/|security-eval/)' \
            || true)"
        if [ -z "$non_queue_core" ] \
            && printf '%s\n' "$changed" \
                | grep -cE '^(scripts/(merge-queue|state-paths)\.sh|tests/merge_queue\.rs)$' >/dev/null; then
            queue_infra_only=1
        fi
    fi

    gate_attempt=$((gate_attempt + 1))
    local log; log="$logs/$(date +%Y%m%d-%H%M%S)-$(branch_key "$branch")-$$-$gate_attempt.log"
    local prepare_finished; prepare_finished="$(date +%s)"
    local lock_what="full gate: $branch"
    if [ "${#batch_branches[@]}" -gt 1 ]; then
        lock_what="${batch_kind:-batch}: ${batch_branches[*]}"
    fi
    acquire_lock "$lock_what" "$branch" "$log"
    local lock_acquired; lock_acquired="$(date +%s)"

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

    note "gating $branch (rebased to $sha on $base; target=$cargo_target_dir; fuzz=$fuzz_mode; scope=$gate_scope; queue-infra=$queue_infra; queue-infra-only=$queue_infra_only); log: $log"
    local gate_started; gate_started="$(date +%s)"
    run_gate "$log" "$fuzz_mode" "$gate_scope" "$queue_infra" "$queue_infra_only" \
        "$cargo_target_dir" "$census_proof_sha"
    local gate_finished; gate_finished="$(date +%s)"
    local gate_took=$((gate_finished - gate_started))

    case "$gate_result" in
        green) ;;
        red | timeout:*)
            # Red candidates cannot land. Release immediately so diagnosis and
            # journal finalization do not delay the next heavyweight user.
            release_lock
            local why="red" extra=""
            [ "$gate_result" != "red" ] && { why="timeout"; extra="${gate_result#timeout: }"; }
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
                    # A dependency stack has an ordered failure boundary. Gate
                    # the first half as one prefix next; green lands that prefix
                    # and exposes the remaining suffix, while another red halves
                    # again. This finds/lands the safe prefix in logarithmic
                    # splits instead of re-gating every member independently.
                    local next_prefix=$(( (${#batch_branches[@]} + 1) / 2 ))
                    set_queue_batch_limit "$f" "$change_id" "$submitted_sha" "$attempt_id" "$next_prefix" || true
                    note "  dependency stack will re-gate prefix of $next_prefix before the blocked suffix"
                elif [ -n "$evict_index" ]; then
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
                return 1
            fi
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

    local current_branch
    current_branch="$(git -C "$root" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
    if [ "$current_branch" = "master" ]; then
        if ! git -C "$root" merge --ff-only "$sha" >/dev/null 2>&1; then
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
    else
        # The main worktree is allowed to be on an agent branch. Do not merge the
        # validated commit into that branch and then journal a false master merge;
        # move only refs/heads/master, guarded by the base SHA that was gated.
        if ! git -C "$root" update-ref refs/heads/master "$sha" "$base" >/dev/null 2>&1; then
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
    local landed_finished; landed_finished="$(date +%s)"
    note "MERGED ${batch_branches[*]} → master @ $sha (gate ${gate_took}s, ${#batch_branches[@]} branch(es))"
    local i bf
    for i in "${!batch_branches[@]}"; do
        record_attempt merged "${batch_branches[$i]}" "$attempt_start" "$prepare_finished" \
            "$lock_acquired" "$gate_started" "$gate_finished" "$landed_finished" sha "$sha" log "$log" \
            change_id "${batch_ids[$i]}" attempt_id "${batch_attempt_ids[$i]}" \
            submitted_sha "${batch_submitted_shas[$i]}" batch "${#batch_branches[@]}" \
            stages "$(stage_summary "$log")"
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
    cmd_sweep || true
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
    # Warm ALL profiles the gate uses in the inactive generation: dev + test in
    # $inactive_target, wasm there as well, and the fail-fast legs in the
    # corresponding -clippy/-check siblings. Nothing copies or renames Cargo
    # outputs between generations. Every Cargo phase must succeed before the
    # selector can be promoted; a prewarm failure remains opportunistic and
    # leaves the current active generation unchanged.
    # Prewarm is opportunistic. A submission arriving after the under-lock
    # recheck must preempt it instead of waiting behind a cold multi-profile
    # build. Put the complete tree, including rustup setup that can wait on its
    # global lock, in its own process group before watching the queue. Terminate
    # only the prewarm process group we started.
    # Match run_gate once for every Cargo phase below. Detached daemons cannot
    # use the checkout's configured compiler wrapper.
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
    set -m
    ( exec "${prewarm_runner[@]}" bash -c '
        set -euo pipefail
        gate_wt="$1"
        reset_inactive="$2"
        inactive_target="$3"
        cd "$gate_wt"
        if [ "$reset_inactive" -eq 1 ]; then
            rm -rf -- "$inactive_target" "${inactive_target}-check" "${inactive_target}-clippy"
            mkdir -p "$inactive_target" "${inactive_target}-check" "${inactive_target}-clippy"
        fi
        tc_bin=""
        if command -v rustup >/dev/null 2>&1; then
            rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
            tc_bin="$(dirname "$(rustup which --toolchain stable rustc)")"
        fi
        export CARGO_TARGET_DIR="$inactive_target" CARGO_INCREMENTAL=0 RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER=
        cargo build --workspace >/dev/null 2>&1
        cargo test --workspace --no-run >/dev/null 2>&1
        if [ -n "$tc_bin" ]; then
            env -u RUSTC -u RUSTFLAGS PATH="$tc_bin:$PATH" \
                cargo build --lib --no-default-features --target wasm32-unknown-unknown >/dev/null 2>&1
        else
            cargo build --lib --no-default-features --target wasm32-unknown-unknown >/dev/null 2>&1
        fi
        CARGO_TARGET_DIR="${inactive_target}-clippy" cargo clippy --workspace --all-targets -- -D warnings >/dev/null 2>&1
        CARGO_TARGET_DIR="${inactive_target}-check" cargo check --workspace --all-targets >/dev/null 2>&1
        [ ! -x scripts/warm-witchy-caches.sh ] || ./scripts/warm-witchy-caches.sh >/dev/null 2>&1
    ' prewarm "$gate_wt" "$reset_inactive" "$inactive_target" ) &
    local prewarm_pid=$!
    active_gate_pgid="$prewarm_pid"
    printf '%s\n' "$prewarm_pid" >"$lock/gate_pgid"
    set +m
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

flush_deferred_sweep() {
    [ "$deferred_sweep" -eq 1 ] || return 0
    cmd_sweep || true
    deferred_sweep=0
}

cmd_run() {
    local once=0
    [ "${1:-}" = "--once" ] && once=1
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
    if ! acquire_coordinator_lock; then
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
    note "coordinator up (pid $$, gate: '$gate_cmd', timeouts: $(gate_timeout_display) total / ${stall_timeout}s stall); state: $qdir"
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
    while :; do
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
            sleep "$poll_interval"
            continue
        fi
        local process_status=0
        process_one "$queue_dir/$f" || process_status=$?
        [ "$process_status" -eq 0 ] && continue
        if [ "$process_status" -eq 2 ]; then
            if [ "$once" -eq 1 ]; then
                note "queue deferred: main master checkout has tracked changes"
                break
            fi
            sleep "$poll_interval"
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
    local swept=0
    while IFS= read -r wt; do
        [ "$wt" = "$root" ] && continue
        [ "$wt" = "$gate_wt" ] && continue
        [ -d "$wt" ] || continue
        local branch; branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
        case "$branch" in '?' | HEAD) continue ;; esac
        # The guard: only branches this queue landed or proved represented, and
        # not re-queued since.
        jq -r 'select(.event=="merged" or .event=="already_merged") | .branch' "$journal" \
            | grep -qx "$branch" || continue
        if ls "$queue_dir"/*.json >/dev/null 2>&1 && \
           jq -r .branch "$queue_dir"/*.json | grep -qx "$branch"; then continue; fi
        # Safety: clean and nothing beyond master. Gated merges land a REBASED
        # sha, so the original commits are often not ancestors — `git cherry`
        # treats patch-equivalent commits (all "-") as merged too.
        [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || continue
        if [ "$(git -C "$wt" rev-list --count "master..HEAD" 2>/dev/null || echo 1)" != "0" ]; then
            git -C "$wt" cherry master HEAD 2>/dev/null | grep -q '^+' && continue
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
    # Journal-merged branches with no worktree left behind are safe to drop too.
    local b
    while IFS= read -r b; do
        git -C "$root" show-ref --verify --quiet "refs/heads/$b" || continue
        git -C "$root" branch -d "$b" >/dev/null 2>&1 && note "sweep: deleted merged branch $b"
    done < <(jq -r 'select(.event=="merged" or .event=="already_merged") | .branch' "$journal" | sort -u)
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
cmd_stats() {
    [ -f "$journal" ] || { note "no journal yet"; exit 0; }
    echo "== outcomes:"
    jq -r '.event' "$journal" | sort | uniq -c | sort -rn
    echo
    echo "== gate seconds (last 10 merged):"
    jq -r 'select(.event=="merged" and .elapsed_s != null) | "\(.ts)  \(.elapsed_s)s  \(.branch)\(if .batch and (.batch != "1") then "  [batch \(.batch)]" else "" end)"' "$journal" | tail -10
    echo
    echo "== repeat offenders (red/timeout more than once):"
    jq -r 'select(.event=="red" or .event=="timeout") | .branch' "$journal" | sort | uniq -c | sort -rn | awk '$1 > 1'
    echo
    echo "== suspected flaky tests (FAILED in a red gate, then absent from later failures):"
    # Pull per-test FAIL/TIMEOUT lines out of red-gate logs; a test that fails in
    # exactly one log but ran in several is flake-shaped. Cheap heuristic, not proof.
    local names
    names="$(jq -r 'select(.event=="red" or .event=="timeout" or .event=="batch_red") | .log // empty' "$journal" \
        | while IFS= read -r lg; do
            [ -f "$lg" ] || continue
            sed "s/$(printf '\033')\[[0-9;]*m//g" "$lg" | grep -E '^[[:space:]]*(FAIL|TIMEOUT) \[' \
                | sed -E 's/^[[:space:]]*(FAIL|TIMEOUT) \[[^]]*\] \([^)]*\) //'
          done | sort | uniq -c | sort -rn)"
    if [ -n "$names" ]; then echo "$names" | head -15; else echo "  (none recorded)"; fi
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
    stats)     cmd_stats ;;
    drop)      shift; cmd_drop "$@" ;;
    resolve)   shift; cmd_resolve "$@" ;;
    sweep)     shift; cmd_sweep "$@" ;;
    with-lock) shift; cmd_with_lock "$@" ;;
    -h | --help | "") sed -n '2,68p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) note "unknown subcommand '${1}' (try submit, status, doctor, run, daemon, migrate-state, drop, sweep, with-lock)"; exit 2 ;;
esac
