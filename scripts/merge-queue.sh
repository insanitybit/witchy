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
# MERGE_QUEUE_STALL_TIMEOUT seconds (default 600) or the whole gate exceeds
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
#   scripts/merge-queue.sh submit [--front] <branch> [note]
#                                                   enqueue a local branch (warns if the
#                                                   diff overlaps another queued branch).
#                                                   --front puts it at the HEAD of the
#                                                   queue (urgent fixes; use sparingly)
#   scripts/merge-queue.sh wait <branch> [secs]     block until the branch reaches a
#                                                   terminal journal event (merged/red/
#                                                   timeout/conflict/blocked/dropped),
#                                                   print it as JSON; exit 0 iff merged.
#                                                   Default timeout 3600s
#   scripts/merge-queue.sh status                   queue + in-flight gate + recent journal (JSON)
#   scripts/merge-queue.sh doctor                   human health check: coordinator alive?
#                                                   lock stale? current stage? log fresh?
#   scripts/merge-queue.sh run [--once]             coordinator loop (--once: drain and exit)
#   scripts/merge-queue.sh daemon                   start the coordinator in a detached session,
#                                                   surviving the launching session; log →
#                                                   scratch/merge-queue/coordinator.log; stop
#                                                   with: kill $(cat .../coordinator.pid)
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
root="$(git -C "$here" worktree list --porcelain | head -1 | sed 's/^worktree //')"
qdir="${MERGE_QUEUE_STATE_DIR:-$root/scratch/merge-queue}"
queue_dir="$qdir/queue"
journal="$qdir/journal.jsonl"
logs="$qdir/logs"
lock="$qdir/gate.lock"
gate_wt="${MERGE_QUEUE_GATE_WT:-$root/.claude/worktrees/merge-gate}"
gate_cmd="${MERGE_QUEUE_GATE_CMD:-./scripts/check.sh}"
gate_timeout="${MERGE_QUEUE_GATE_TIMEOUT:-2700}"
stall_timeout="${MERGE_QUEUE_STALL_TIMEOUT:-600}"

mkdir -p "$queue_dir" "$logs"

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
note() { printf 'merge-queue: %s\n' "$*" >&2; }
strip_ansi() { sed "s/$(printf '\033')\[[0-9;]*m//g"; }

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

# Run the gate in its own process group with a stall/overall-timeout monitor.
# Sets gate_result to "green", "red", or "timeout: <why>". Never returns nonzero.
gate_result=""
run_gate() { # run_gate <log> [fuzz-mode] [gate-scope]
    local log="$1"
    local fuzz_mode="${2:-full}"
    local gate_scope="${3:-all}"
    local start; start="$(date +%s)"
    # `set -m` puts the background job in its own process group, so a timeout
    # can kill the WHOLE cargo/nextest tree, not just the top shell.
    #
    # CARGO_INCREMENTAL=0 is LOAD-BEARING, not hygiene (fb05dd34 landed it
    # without a recorded reason): ~/.cargo/config.toml sets
    # `rustc-wrapper = sccache`, and sccache REJECTS incremental compiles —
    # `-C incremental=…` invocations exit nonzero, so an incremental gate
    # build fails outright (measured 2026-07-15; see
    # scratch/gate-perf-2026-07-15.md). Do not flip this while sccache is the
    # global wrapper.
    set -m
    # Test execution runs at nextest's normal width. The macOS dyld stall that
    # once motivated NEXTEST_TEST_THREADS=4 here was a DISCOVERY problem —
    # CPU-count concurrent cold first-execs of ~100 MB --list binaries — and is
    # solved at the source by scripts/nextest-list-wrapper.sh
    # (.config/nextest.toml); by run time every binary is loader-warm. Bounding
    # execution as well doubled gate wall-clock (measured 2026-07-16: ~20.6 min
    # at width 4 vs the historical 8-10 min).
    ( cd "$gate_wt" && exec env CARGO_INCREMENTAL=0 NEXTEST_STATUS_LEVEL=pass "WITCHY_GATE_FUZZ=$fuzz_mode" "WITCHY_GATE_SCOPE=$gate_scope" bash -c "$gate_cmd" ) >"$log" 2>&1 &
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
        # Log silence alone is NOT a hang. The gate legitimately goes quiet for
        # minutes — from t+0, since tests are now the FIRST stage: nextest
        # compiles the `test` profile (separate artifacts from the `dev`-profile
        # build/clippy artifacts) and then enumerates+starts tests,
        # all before the first streamed `PASS` line — and under CPU contention that
        # silent window blew the 300s stall clock, killing HEALTHY gates (every
        # observed "no log output" timeout was this false positive, never a real
        # hang; the whole-gate limit above already backstops a true wedge). So
        # gate liveness on CPU, not log writes: a process group burning CPU is
        # compiling/testing; only silence WITH no CPU is a genuine stall. A real
        # hang (deadlock/blocked syscall) consumes no CPU, so it still trips.
        if [ "$age" -gt "$stall_timeout" ]; then
            # A busy group that has been silent for a NORMAL compile-window length
            # is fine (this is the false-positive fix). But a group that is busy
            # AND silent for far longer than any compile+enumeration takes is a
            # CPU-burning runaway (e.g. a busy-spin infinite loop in a test) — kill
            # it well before the 45-min whole-gate ceiling so it doesn't block the
            # serialized queue that long. `busy_silence_max` = 3× the stall window
            # (default 1800s), comfortably above a cold test-profile compile even
            # under contention, far below GATE_TIMEOUT.
            local busy_silence_max="${MERGE_QUEUE_BUSY_SILENCE_MAX:-$((stall_timeout * 3))}"
            if group_is_busy "$gpid" && [ "$age" -le "$busy_silence_max" ]; then
                continue
            fi
            if group_is_busy "$gpid"; then
                why="no log output for ${age}s despite a busy process group — runaway (MERGE_QUEUE_BUSY_SILENCE_MAX=${busy_silence_max})"
            else
                why="no log output for ${age}s and process group idle (MERGE_QUEUE_STALL_TIMEOUT=${stall_timeout})"
            fi
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
    local front=0
    [ "${1:-}" = "--front" ] && { front=1; shift; }
    local branch="${1:?usage: merge-queue.sh submit [--front] <branch> [note]}"
    local msg="${2:-}"
    git -C "$root" rev-parse --verify --quiet "refs/heads/$branch" >/dev/null \
        || { note "no local branch '$branch'"; exit 2; }
    # Submit-time conflict pre-check (instant, in-memory): a branch that cannot
    # even merge with current master would burn a queue slot only to journal
    # `conflict` minutes later. git merge-tree does a real 3-way merge without
    # touching any worktree. Advisory-fail: refuse with the reason; --force to
    # override (e.g. master is about to change under you anyway).
    if [ "${MERGE_QUEUE_SKIP_PRECHECK:-}" != "1" ]; then
        if ! git -C "$root" merge-tree --write-tree --name-only master "refs/heads/$branch" >/dev/null 2>&1; then
            note "REFUSED: $branch does not merge cleanly with current master — rebase it first"
            note "(the gate would only journal 'conflict'; MERGE_QUEUE_SKIP_PRECHECK=1 to submit anyway)"
            exit 1
        fi
    fi

    # Overlap warning (advisory, never blocking): if a queued branch touches the
    # same files, the later one will likely need a semantic rebase after the
    # earlier merges — worth knowing before you walk away.
    local qf other overlap
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
    # Queue position is the filename's sort order. Normal: epoch seconds.
    # --front: sort before every epoch timestamp (queue files start with a digit).
    local stamp; stamp="$(date +%s)"
    [ "$front" -eq 1 ] && stamp="0front-$stamp"
    local fname; fname="$stamp-$(echo "$branch" | tr '/' '~').json"
    jq -cn --arg branch "$branch" --arg ts "$(now)" \
           --arg sha "$(git -C "$root" rev-parse "refs/heads/$branch")" \
           --arg by "${USER:-unknown}" --arg note "$msg" \
           '{branch: $branch, sha: $sha, submitted: $ts, by: $by, note: $note}' \
        >"$queue_dir/$fname"
    record submitted "$branch" by "${USER:-unknown}"
    note "queued $branch ($fname)"
    local cpid; cpid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null; then
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

    # BATCHING: stack further queued branches onto this candidate so ONE gate
    # validates them all. A branch joins the batch if it rebases CLEANLY onto
    # the stack — textual overlap that rebases fine is allowed (nearly every
    # language branch touches example_tests.rs; requiring disjoint files
    # forfeited batching exactly where queues run deepest). A red batch
    # re-queues every member for individual gating (.nobatch), so nothing is
    # ever merged unvalidated and no member is blamed by association.
    local batch_files=("$f") batch_branches=("$branch")
    local qf cand cdiff csha tip
    for qf in "$queue_dir"/*.json; do
        [ -f "$qf" ] || continue
        [ "$qf" = "$f" ] && continue
        # A member of a failed batch gates alone until it individually passes
        # or fails (the .nobatch marker set on batch_red).
        [ -e "$f.nobatch" ] && break
        [ -e "$qf.nobatch" ] && continue
        [ "${#batch_branches[@]}" -ge "${MERGE_QUEUE_BATCH_MAX:-5}" ] && break
        cand="$(jq -r .branch "$qf")"
        csha="$(git -C "$root" rev-parse --verify --quiet "refs/heads/$cand")" || continue
        cdiff="$(git -C "$root" diff --name-only "master...$cand" 2>/dev/null | sort -u)"
        [ -n "$cdiff" ] || continue
        # Rebase the candidate's SHA (detached — never moves the agent's branch
        # ref) onto the current stack tip. On failure, abort returns HEAD to the
        # candidate sha, so re-detach onto the saved tip either way it fails.
        tip="$(git -C "$gate_wt" rev-parse HEAD)"
        if git -C "$gate_wt" rebase "$tip" "$csha" >/dev/null 2>&1; then
            batch_files+=("$qf"); batch_branches+=("$cand")
        else
            git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
            git -C "$gate_wt" checkout --detach --quiet "$tip"
        fi
    done
    if [ "${#batch_branches[@]}" -gt 1 ]; then
        note "batched ${#batch_branches[@]} branches into one gate: ${batch_branches[*]}"
        echo "batch: ${batch_branches[*]}" >"$lock/what"
    fi
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
    # from this list), and scratch//security-eval/ (gitignored). Everything
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
        unsafe_paths="$(echo "$changed" | grep -vE '^(rfcs/|wiki/|bugs/|scratch/|security-eval/)' || true)"
    fi
    if [ -n "$changed" ] && [ -z "$unsafe_paths" ] \
        && ! echo "$changed" | grep -cx 'rfcs/performance-modes\.md' >/dev/null; then
        gate_scope="docs"
    fi

    local log; log="$logs/$(date +%Y%m%d-%H%M%S)-$(echo "$branch" | tr '/' '~').log"
    echo "$log" >"$lock/log"
    note "gating $branch (rebased to $sha on $base; fuzz=$fuzz_mode; scope=$gate_scope); log: $log"
    run_gate "$log" "$fuzz_mode" "$gate_scope"
    release_lock
    local took=$(( $(date +%s) - t0 ))

    case "$gate_result" in
        green) ;;
        red | timeout:*)
            local why="red" extra=""
            [ "$gate_result" != "red" ] && { why="timeout"; extra="${gate_result#timeout: }"; }
            if [ "${#batch_branches[@]}" -gt 1 ]; then
                # A red BATCH indicts no one member: keep every queue file so
                # each re-gates individually (batching only re-engages when a
                # solo branch is at the head with others behind it).
                [ "$why" = "red" ] && extra="$(failure_summary "$log")"
                note "batch of ${#batch_branches[@]} is $(echo "$why" | tr a-z A-Z) after ${took}s — $extra"
                note "  re-queueing members for individual gates; log: $log"
                record batch_red "$branch" members "${batch_branches[*]}" log "$log" \
                    elapsed_s "$took" reason "$extra"
                # Mark every member no-batch so the retry gates them one by one.
                local bf; for bf in "${batch_files[@]}"; do touch "$bf.nobatch"; done
                return 1
            fi
            [ "$why" = "red" ] && extra="$(failure_summary "$log")"
            note "$branch is $(echo "$why" | tr a-z A-Z) after ${took}s — $extra"
            note "  log: $log"
            record "$why" "$branch" sha "$sha" log "$log" elapsed_s "$took" \
                reason "$extra" stages "$(stage_summary "$log")"
            rm -f "$f"
            return 0
            ;;
    esac

    # TEST-MODE SAFETY: an isolated state dir isolates the queue/journal but
    # NOT the merge target — without this guard a harness test would ff the
    # REAL master (it did, once). Tests must opt in explicitly to merging.
    if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ] && [ "${MERGE_QUEUE_ALLOW_MERGE:-}" != "1" ]; then
        note "test state dir active and MERGE_QUEUE_ALLOW_MERGE!=1 — gate was GREEN but skipping the real merge"
        local vi
        for vi in "${!batch_branches[@]}"; do
            record validated "${batch_branches[$vi]}" sha "$sha" log "$log" reason "test mode: merge skipped"
            rm -f "${batch_files[$vi]}" "${batch_files[$vi]}.nobatch"
        done
        return 0
    fi

    if [ "$(git -C "$root" rev-parse master)" != "$base" ]; then
        note "master moved during the gate; requeueing $branch for a fresh rebase"
        record requeued "$branch" sha "$sha" reason "master moved"
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
            record blocked "$branch" sha "$sha" reason "ff-merge failed in main worktree" log "$log"
            rm -f "$f"
            return 0
        fi
    else
        # The main worktree is allowed to be on an agent branch. Do not merge the
        # validated commit into that branch and then journal a false master merge;
        # move only refs/heads/master, guarded by the base SHA that was gated.
        if ! git -C "$root" update-ref refs/heads/master "$sha" "$base" >/dev/null 2>&1; then
            note "fast-forward of refs/heads/master to $sha FAILED (master moved or ref lock failed)."
            record requeued "$branch" sha "$sha" reason "master ref update failed"
            return 1
        fi
    fi
    note "MERGED ${batch_branches[*]} → master @ $sha (gate ${took}s, ${#batch_branches[@]} branch(es))"
    local i bf
    for i in "${!batch_branches[@]}"; do
        record merged "${batch_branches[$i]}" sha "$sha" log "$log" elapsed_s "$took" \
            batch "${#batch_branches[@]}" stages "$(stage_summary "$log")"
        bf="${batch_files[$i]}"
        rm -f "$bf" "$bf.nobatch"
    done
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
    local m; m="$(git -C "$root" rev-parse master)"
    [ -f "$qdir/prewarmed" ] && [ "$(cat "$qdir/prewarmed")" = "$m" ] && return 0
    acquire_lock "prewarm: master @ ${m:0:9}"
    # Re-check under the lock — a submit may have raced us.
    if ls "$queue_dir"/*.json >/dev/null 2>&1; then release_lock; return 0; fi
    note "idle: prewarming gate worktree at master ${m:0:9}"
    git -C "$gate_wt" rebase --abort >/dev/null 2>&1 || true
    git -C "$gate_wt" checkout --detach --quiet "$m" 2>/dev/null || { release_lock; return 0; }
    # Warm ALL profiles the gate uses: dev (build), test (nextest), the wasm
    # playground target, and — when it exists — the separate clippy check dir
    # (target-clippy, where check.sh's background clippy leg runs; check.sh
    # CoW-seeds it on first use). Without this, each cold profile adds 30-130s
    # to the gate wall-clock. The clippy warm-up uses the EXACT gate flags
    # (`-- -D warnings`) so its fingerprints match the gate's; `|| true` keeps a
    # master-side lint from failing the prewarm. The wasm build needs the rustup
    # toolchain's std (same PATH trick as check.sh).
    local tc_bin=""
    if command -v rustup >/dev/null 2>&1; then
        rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
        tc_bin="$(dirname "$(rustup which --toolchain stable rustc)")"
    fi
    ( cd "$gate_wt" \
        && cargo build --workspace >/dev/null 2>&1 \
        && cargo test --workspace --no-run >/dev/null 2>&1 \
        && { if [ -n "$tc_bin" ]; then
                 env -u RUSTC -u RUSTFLAGS PATH="$tc_bin:$PATH" \
                     cargo build --lib --no-default-features --target wasm32-unknown-unknown >/dev/null 2>&1
             else
                 cargo build --lib --no-default-features --target wasm32-unknown-unknown >/dev/null 2>&1
             fi || true; } \
        && { [ -d target-clippy ] \
                 && CARGO_TARGET_DIR=target-clippy cargo clippy --workspace --all-targets -- -D warnings >/dev/null 2>&1 \
                 || true; } \
        && { [ -x scripts/warm-witchy-caches.sh ] && ./scripts/warm-witchy-caches.sh >/dev/null 2>&1 || true; } ) \
        && echo "$m" >"$qdir/prewarmed" || true
    release_lock
}

cmd_run() {
    local once=0
    [ "${1:-}" = "--once" ] && once=1
    # Only the PERSISTENT loop owns coordinator.pid. An ad-hoc `run --once`
    # used to clobber it, then exit — leaving a dead pid in the file, so
    # doctor/submit reported NO COORDINATOR while the real daemon was alive,
    # and the natural reaction (start another daemon) produced TWO coordinators
    # racing the queue. --once also refuses to run alongside a live daemon.
    local cpid; cpid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$cpid" ] && [ "$cpid" != "$$" ] && kill -0 "$cpid" 2>/dev/null; then
        if [ "$once" -eq 1 ]; then
            note "a persistent coordinator (pid $cpid) is already running — it will drain the queue; not starting a --once run"
            exit 0
        fi
        note "a coordinator is already running (pid $cpid); refusing to start a second"
        exit 1
    fi
    [ "$once" -eq 0 ] && echo "$$" >"$qdir/coordinator.pid"
    note "coordinator up (pid $$, gate: '$gate_cmd', timeouts: ${gate_timeout}s total / ${stall_timeout}s stall); state: $qdir"
    while :; do
        local f
        f="$(ls -1 "$queue_dir" 2>/dev/null | sort | head -1 || true)"
        if [ -z "$f" ]; then
            if [ "$once" -eq 1 ]; then note "queue drained"; break; fi
            prewarm_gate
            sleep 15
            continue
        fi
        process_one "$queue_dir/$f" || sleep 5
    done
}

# Remove worktrees whose branch this queue has MERGED (per journal.jsonl) and
# whose tree is clean + fully contained in master. Journal-merged is the load-
# bearing guard: a FRESH agent worktree (branch at master, no commits yet) is
# indistinguishable from a merged one by ahead-count alone — sweeping on that
# heuristic would delete a working agent's checkout. Never touches the main
# worktree or the gate worktree. Each removal frees a multi-GB target/.
cmd_sweep() {
    if [ ! -f "$journal" ]; then note "sweep: no journal; nothing merged yet"; return 0; fi
    local swept=0
    while IFS= read -r wt; do
        [ "$wt" = "$root" ] && continue
        [ "$wt" = "$gate_wt" ] && continue
        [ -d "$wt" ] || continue
        local branch; branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
        case "$branch" in '?' | HEAD) continue ;; esac
        # The guard: only branches this queue merged, and not re-queued since.
        jq -r 'select(.event=="merged") | .branch' "$journal" | grep -qx "$branch" || continue
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
    done < <(jq -r 'select(.event=="merged") | .branch' "$journal" | sort -u)
    note "sweep: removed $swept worktree(s)"
}

# Block until <branch> reaches a terminal journal event newer than this call
# (merged/red/timeout/conflict/blocked/dropped), print that event as JSON.
# Exit 0 iff merged. For agents: `submit X && wait X` replaces polling loops.
cmd_wait() {
    local branch="${1:?usage: merge-queue.sh wait <branch> [timeout-secs]}"
    local budget="${2:-3600}"
    local start_line=0
    [ -f "$journal" ] && start_line="$(wc -l <"$journal" | tr -d ' ')"
    local waited=0 ev
    while [ "$waited" -le "$budget" ]; do
        if [ -f "$journal" ]; then
            ev="$(tail -n "+$((start_line + 1))" "$journal" | jq -c --arg b "$branch" \
                'select(.branch==$b) | select(.event=="merged" or .event=="red" or .event=="timeout" or .event=="conflict" or .event=="blocked" or .event=="dropped")' \
                | tail -1)"
            if [ -n "$ev" ]; then
                echo "$ev"
                [ "$(echo "$ev" | jq -r .event)" = "merged" ] && return 0 || return 1
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

# After a `blocked` event (gate green, ff-merge refused by the main worktree)
# the operator merges manually — which leaves the journal's last word as
# "blocked" and misleads every agent reading it. `resolve` closes the record:
# it verifies the journaled sha actually IS on master, then journals `merged`.
cmd_resolve() {
    local branch="${1:?usage: merge-queue.sh resolve <branch>}"
    local sha
    sha="$(jq -r --arg b "$branch" 'select(.event=="blocked" and .branch==$b) | .sha' "$journal" 2>/dev/null | tail -1)"
    [ -n "$sha" ] || { note "no blocked event for '$branch' in the journal"; exit 2; }
    if ! git -C "$root" merge-base --is-ancestor "$sha" master; then
        note "$sha is NOT on master — merge it first: git merge --ff-only $sha"
        exit 1
    fi
    record merged "$branch" sha "$sha" via "manual ff after blocked"
    note "journaled merged for $branch @ $sha"
    cmd_sweep || true
}

cmd_daemon() {
    local cpid; cpid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null; then
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
        nohup setsid -f "$root/scripts/merge-queue.sh" run \
            >>"$qdir/coordinator.log" 2>&1 </dev/null &
    elif command -v perl >/dev/null 2>&1; then
        # macOS has no setsid(1), but its system Perl exposes POSIX::setsid.
        # Fork once so the child cannot be a process-group leader, then replace
        # it with the coordinator. All descriptors are already detached below.
        nohup perl -MPOSIX -e '
            my $pid = fork();
            defined $pid or die "fork: $!\n";
            exit 0 if $pid;
            defined POSIX::setsid() or die "setsid: $!\n";
            exec @ARGV;
            die "exec: $!\n";
        ' "$root/scripts/merge-queue.sh" run >>"$qdir/coordinator.log" 2>&1 </dev/null &
    else
        note "daemon requires setsid(1) or Perl POSIX::setsid to detach safely"
        return 1
    fi
    disown || true
    sleep 1
    cpid="$(cat "$qdir/coordinator.pid" 2>/dev/null || true)"
    if [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null; then
        note "coordinator daemon started (pid $cpid); log: $qdir/coordinator.log; stop: kill $cpid"
    else
        note "daemon failed to start — see $qdir/coordinator.log"
        return 1
    fi
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
    wait)      shift; cmd_wait "$@" ;;
    stats)     cmd_stats ;;
    resolve)   shift; cmd_resolve "$@" ;;
    sweep)     shift; cmd_sweep "$@" ;;
    with-lock) shift; cmd_with_lock "$@" ;;
    -h | --help | "") sed -n '2,68p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) note "unknown subcommand '${1}' (try submit, status, doctor, run, daemon, sweep, with-lock)"; exit 2 ;;
esac
