#!/usr/bin/env bash
# Reclaim disk from stale per-worktree cargo build caches.
#
# Agents accumulate multi-GB `target*` dirs per worktree and cargo never GCs
# them, so free space drifts down until builds start failing. Deleting a build
# cache destroys no source and no history -- it costs a rebuild, nothing else.
#
# Two things this script knows that a `du | sort | rm` does not:
#
#   * `du` LIES about worktree caches. `scripts/worktree-warm.sh` seeds a
#     worktree by APFS-CoW-cloning `target/`, so a plain `target` dir reports
#     its full size while sharing almost every block with the origin. Deleting
#     one reports gigabytes and frees ~nothing. A `target-<agent>` dir was
#     built fresh and does free what it claims. Judge by `df`, and prefer the
#     suffixed dirs -- which is why they are the default class.
#   * A cache with an open file handle is a LIVE build. Removing it corrupts
#     another agent's in-flight compile.
#
# Three classes of cache are considered:
#
#   * `target*` dirs inside registered worktrees (in the repo or under
#     /private/tmp/witchy-*), judged by the worktree's branch state.
#   * Standalone `/private/tmp/witchy-*` caches that no worktree references
#     and that are not checkouts (no `.git`): the `CARGO_TARGET_DIR`s agents
#     create per task and abandon. Nothing can vouch for those, so they are
#     reclaimable only after --orphan-min-age-hours (default 48) with no open
#     handle. They are the bulk of the disk on a busy day.
#   * `state/mbx/store-<version>` build-cache stores for an mbx version the
#     checkout no longer pins. An mbx upgrade starts a fresh store and nothing
#     removes the old one (53 GB for 1.16.0 on 2026-09-24). A worktree still on
#     an older base keeps using its store, so a superseded store is reclaimable
#     only once nothing in it has been touched for --min-age-hours and no
#     handle is open.
#
# Default is a dry run. Nothing is deleted without --apply.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
main="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)"
main="$(dirname "$main")"

apply=0
min_age_hours="${RECLAIM_MIN_AGE_HOURS:-6}"
orphan_min_age_hours="${RECLAIM_ORPHAN_MIN_AGE_HOURS:-48}"
include_unlanded=0
include_clones=0
include_queued=0
pool_min_age_hours="${RECLAIM_POOL_MIN_AGE_HOURS:-24}"
keep_free_gb=0

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; $d'
    cat <<'USAGE'
Usage: scripts/reclaim-build-caches.sh [options]

  --apply                 actually delete (default: dry run)
  --min-age-hours N       skip worktree caches touched within N hours (default 6)
  --orphan-min-age-hours N
                          skip standalone /private/tmp/witchy-* caches touched
                          within N hours (default 48; no worktree can vouch for
                          them, so the bar is higher)
  --include-unlanded      also consider worktrees whose branch has not landed
                          (safe -- costs a rebuild -- but disruptive to an
                          agent who is mid-task, so it is opt-in)
  --include-clones        also consider plain `target` CoW clones, which
                          usually free far less than their apparent size
  --include-queued        also consider worktrees whose branch is sitting in
                          the merge queue (their owner may need a warm tree to
                          fix a red gate, so they are held back by default)
  --pool-min-age-hours N  keep the cache of a free worktree-pool slot
                          (RFC-0171) until it has sat free N hours (default 24)
  --keep-free-gb N        stop once free space reaches N GB
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) apply=1 ;;
        --min-age-hours) min_age_hours="$2"; shift ;;
        --orphan-min-age-hours) orphan_min_age_hours="$2"; shift ;;
        --include-unlanded) include_unlanded=1 ;;
        --include-clones) include_clones=1 ;;
        --include-queued) include_queued=1 ;;
        --pool-min-age-hours) pool_min_age_hours="$2"; shift ;;
        --keep-free-gb) keep_free_gb="$2"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

free_mb() { df -m / | tail -1 | awk '{print $4}'; }

# One lsof sweep: repeating it per candidate would be slow enough to race a
# build that starts midway through.
# Both prefixes in one sweep: an out-of-tree cache with an open handle is just
# as live as an in-tree one.
# The sanity check is on lsof itself, not on what it found: an idle machine
# legitimately has no open build path, and that is exactly when reclaiming
# is safest. A broken or blocked lsof prints nothing at all.
lsof_output="$(lsof -n 2>/dev/null || true)"
if [ -z "$lsof_output" ]; then
    echo "reclaim: WARNING: lsof returned nothing at all (blocked or broken)." >&2
    echo "reclaim: cannot tell live caches from idle ones -- treating every cache as LIVE." >&2
    echo "reclaim: re-run when lsof works rather than deleting blind." >&2
    exit 1
fi
open_paths="$(printf '%s\n' "$lsof_output" \
    | grep -oE "$main/[^ ]*target[^/ ]*|/private/tmp/witchy-[^/ ]+(/target[^/ ]*)?" \
    | sort -u || true)"

# A branch waiting in the queue may need a fast fix-up if its gate goes red,
# and a cold rebuild is the worst moment to hand its owner. Gating itself is
# unaffected either way -- the coordinator builds in the merge-gate worktree,
# which is skipped below.
queued_branches=""
if [ "$include_queued" -eq 0 ]; then
    queued_branches="$("$main/scripts/merge-queue.sh" status 2>/dev/null \
        | sed -n 's/.*"branch"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | sort -u || true)"
fi

now="$(date +%s)"
. "$root/scripts/state-paths.sh"
. "$root/scripts/worktree-pool.sh"
pool_state="$(worktree_pool_state_dir "$main")"
start_free="$(free_mb)"
printf 'reclaim: %s MB free before\n\n' "$start_free"
printf '%9s  %-8s  %s\n' SIZE_MB STATE PATH

freed_candidates=0
declare -a doomed=()

while IFS= read -r wt; do
    [ -n "$wt" ] || continue
    [ "$wt" = "$main" ] && continue
    # The merge-gate worktree holds the coordinator's warm prewarm caches; the
    # gate rebuilds from cold without them and every queued branch pays for it.
    case "$wt" in *"/merge-gate") continue ;; esac

    # Ask the cheap question first. Most registered worktrees have no build
    # cache at all -- they were never built in, or theirs was already
    # reclaimed -- and interrogating git about every one of them cost more
    # than the whole rest of the run put together.
    has_cache=0
    for t in "$wt"/target*; do
        if [ -d "$t" ]; then has_cache=1; break; fi
    done
    [ "$has_cache" -eq 1 ] || continue

    # A registered worktree whose checkout is gone (the directory was wiped
    # and reused, leaving only caches) has no branch state to consult, and
    # asking git about it aborts the whole run. Judge its caches by age
    # alone, on the orphan threshold, like a standalone cache.
    if git -C "$wt" rev-parse --git-dir >/dev/null 2>&1; then
        gone=0
        branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo DETACHED)"
        dirty="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
        # The queue rebases, so a landed branch is never an ancestor of master --
        # patch-equivalence is the only honest test.
        unlanded="$(git -C "$wt" cherry master HEAD 2>/dev/null | grep -c '^+' || true)"
        unlanded="${unlanded//[!0-9]/}"; unlanded="${unlanded:-0}"
    else
        gone=1
        branch=GONE
        dirty=0
        unlanded=0
    fi

    # A free pool slot (RFC-0171) is detached at a landed master, so branch
    # state alone would call its cache stale. Its age is how long it has sat
    # free; worktree-create.sh evicts a slot whose cache is gone.
    pool_age_hours=""
    if [ "$gone" -eq 0 ] && [ "$branch" = HEAD ] \
        && pool_slot="$(worktree_pool_slot_of "$main" "$wt")" \
        && [ -f "$pool_state/free/$pool_slot" ]; then
        pool_freed="$(cat "$pool_state/free/$pool_slot" 2>/dev/null || echo 0)"
        case "$pool_freed" in '' | *[!0-9]*) pool_freed=0 ;; esac
        pool_age_hours=$(( (now - pool_freed) / 3600 ))
    fi
    for t in "$wt"/target*; do
        [ -d "$t" ] || continue
        base="$(basename "$t")"
        state=""
        if grep -qF -- "$t" <<<"$open_paths"; then
            state=LIVE
        elif [ -n "$pool_age_hours" ] && [ "$pool_age_hours" -lt "$pool_min_age_hours" ]; then
            # RFC-0171: a free pool slot keeps its cache for the next task.
            state="POOL:${pool_age_hours}h"
        elif [ "$base" = target ] && [ "$include_clones" -eq 0 ]; then
            state=CLONE
        elif [ "$gone" -eq 1 ] && [ "$(( (now - $(stat -f %m "$t")) / 3600 ))" -lt "$orphan_min_age_hours" ]; then
            state=RECENT
        elif [ "$gone" -eq 0 ] && [ "$(( (now - $(stat -f %m "$t")) / 3600 ))" -lt "$min_age_hours" ]; then
            state=RECENT
        elif [ -n "$queued_branches" ] && grep -qxF -- "$branch" <<<"$queued_branches"; then
            state=QUEUED
        elif [ "$unlanded" -gt 0 ] && [ "$include_unlanded" -eq 0 ]; then
            state=UNLANDED
        elif [ "$dirty" -gt 0 ] && [ "$include_unlanded" -eq 0 ]; then
            state=DIRTY
        else
            state=STALE
        fi
        if [ "$state" = STALE ]; then
            # Size only what we might actually delete. `du` over a multi-GB CoW
            # target tree costs seconds, and on a busy checkout nearly every
            # cache is held back -- sizing all of them made the common
            # "nothing to do" run take minutes while competing with a live gate
            # for IO, which is the exact moment you least want to spend it.
            size="$(du -sm "$t" 2>/dev/null | cut -f1 || echo 0)"
            printf '%9s  %-8s  %s\n' "$size" "$state" "${t#"$main"/}"
            # Carry the mtime so the deletion order is oldest-first: the least
            # recently touched cache is the one whose owner is least likely to
            # be mid-task and pay for the rebuild.
            doomed+=("$(stat -f %m "$t")	$t")
            freed_candidates=$((freed_candidates + size))
        else
            printf '%9s  %-8s  %s\n' "-" "$state" "${t#"$main"/}"
        fi
    done
done < <(git -C "$main" worktree list --porcelain | awk '/^worktree /{print $2}')

# Standalone caches: a `CARGO_TARGET_DIR` under /private/tmp/witchy-* that no
# registered worktree contains and that is not itself a checkout. No branch
# state can vouch for one, so the only evidence is age and open handles.
registered="$(git -C "$main" worktree list --porcelain | awk '/^worktree /{print $2}')"
for t in /private/tmp/witchy-*; do
    [ -d "$t" ] || continue
    [ -e "$t/.git" ] && continue
    # Only build caches: a cargo target has a profile dir (or the CACHEDIR
    # marker); anything else under the prefix (probe scratch, logs) is not
    # ours to judge and is skipped.
    if [ ! -d "$t/debug" ] && [ ! -d "$t/release" ] && [ ! -e "$t/CACHEDIR.TAG" ]; then
        continue
    fi
    grep -qxF -- "$t" <<<"$registered" && continue
    state=""
    if grep -qF -- "$t" <<<"$open_paths"; then
        state=LIVE
    elif [ "$(( (now - $(stat -f %m "$t")) / 3600 ))" -lt "$orphan_min_age_hours" ]; then
        state=RECENT
    else
        state=ORPHAN
    fi
    if [ "$state" = ORPHAN ]; then
        size="$(du -sm "$t" 2>/dev/null | cut -f1 || echo 0)"
        printf '%9s  %-8s  %s\n' "$size" "$state" "$t"
        doomed+=("$(stat -f %m "$t")	$t")
        freed_candidates=$((freed_candidates + size))
    else
        printf '%9s  %-8s  %s\n' "-" "$state" "$t"
    fi
done

# Superseded mbx stores. The pinned version is read from the main checkout,
# which is what new worktrees and the shims use.
pinned_mbx="$(sed -n 's/^MBX_VERSION="\([^"]*\)".*/\1/p' "$main/scripts/mbx-install.sh" | head -1)"
for t in "$main"/state/mbx/store-*; do
    [ -d "$t" ] || continue
    [ -n "$pinned_mbx" ] || break
    [ "$(basename "$t")" = "store-$pinned_mbx" ] && continue
    # The store root's own mtime does not move on a cache hit; its top-level
    # entries (scheduler, incremental, actions) do.
    newest="$(stat -f %m "$t" "$t"/* 2>/dev/null | sort -n | tail -1)"
    if grep -qF -- "$t" <<<"$lsof_output"; then
        state=LIVE
    elif [ "$(( (now - ${newest:-$now}) / 3600 ))" -lt "$min_age_hours" ]; then
        state=RECENT
    else
        state=OLDMBX
    fi
    if [ "$state" = OLDMBX ]; then
        size="$(du -sm "$t" 2>/dev/null | cut -f1 || echo 0)"
        printf '%9s  %-8s  %s\n' "$size" "$state" "${t#"$main"/}"
        doomed+=("${newest:-0}	$t")
        freed_candidates=$((freed_candidates + size))
    else
        printf '%9s  %-8s  %s\n' "-" "$state" "${t#"$main"/}"
    fi
done

printf '\nreclaim: %d cache(s) reclaimable, %d MB apparent\n' "${#doomed[@]}" "$freed_candidates"

if [ "$apply" -eq 0 ]; then
    echo "reclaim: dry run -- pass --apply to delete."
    exit 0
fi

while IFS=$'\t' read -r _mtime t; do
    [ -n "$t" ] || continue
    if [ "$keep_free_gb" -gt 0 ] && [ "$(free_mb)" -ge $((keep_free_gb * 1024)) ]; then
        echo "reclaim: reached --keep-free-gb $keep_free_gb; stopping."
        break
    fi
    # Belt and braces on a destructive loop: every path here came from
    # `git worktree list`, but a bug upstream of this line must not become an
    # rm -rf outside the repo.
    case "$t" in
        *..*) echo "reclaim: refusing traversal in path: $t" >&2; exit 3 ;;
    esac
    case "$t" in
        "$main"/*/target|"$main"/*/target-*) ;;
        /private/tmp/witchy-*/target|/private/tmp/witchy-*/target-*) ;;
        /private/tmp/witchy-*)
            # A standalone cache: never a registered worktree, never a checkout.
            if grep -qxF -- "$t" <<<"$registered" || [ -e "$t/.git" ]; then
                echo "reclaim: refusing to remove a checkout: $t" >&2; exit 3
            fi ;;
        "$main"/state/mbx/store-*)
            [ "$t" = "$main/state/mbx/store-$pinned_mbx" ] && {
                echo "reclaim: refusing to remove the pinned mbx store: $t" >&2; exit 3; }
            # mbx stores its entries read-only; rm -rf alone fails on them.
            chmod -R u+w "$t" ;;
        *) echo "reclaim: refusing to remove unrecognised path: $t" >&2; exit 3 ;;
    esac
    rm -rf "$t"
    printf 'reclaim: removed %-58s  free now %s MB\n' "${t#"$main"/}" "$(free_mb)"
done < <(printf '%s\n' ${doomed[0]+"${doomed[@]}"} | sort -n)

end_free="$(free_mb)"
printf '\nreclaim: %s MB -> %s MB free (+%s MB actually reclaimed)\n' \
    "$start_free" "$end_free" "$((end_free - start_free))"
