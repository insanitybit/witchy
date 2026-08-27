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
# Default is a dry run. Nothing is deleted without --apply.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
main="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)"
main="$(dirname "$main")"

apply=0
min_age_hours="${RECLAIM_MIN_AGE_HOURS:-6}"
include_unlanded=0
include_clones=0
keep_free_gb=0

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; $d'
    cat <<'USAGE'
Usage: scripts/reclaim-build-caches.sh [options]

  --apply                 actually delete (default: dry run)
  --min-age-hours N       skip caches touched within N hours (default 6)
  --include-unlanded      also consider worktrees whose branch has not landed
                          (safe -- costs a rebuild -- but disruptive to an
                          agent who is mid-task, so it is opt-in)
  --include-clones        also consider plain `target` CoW clones, which
                          usually free far less than their apparent size
  --keep-free-gb N        stop once free space reaches N GB
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) apply=1 ;;
        --min-age-hours) min_age_hours="$2"; shift ;;
        --include-unlanded) include_unlanded=1 ;;
        --include-clones) include_clones=1 ;;
        --keep-free-gb) keep_free_gb="$2"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

free_mb() { df -m / | tail -1 | awk '{print $4}'; }

# One lsof sweep: repeating it per candidate would be slow enough to race a
# build that starts midway through.
open_paths="$(lsof -n 2>/dev/null | grep -o "$main/[^ ]*target[^/ ]*" | sort -u || true)"
if [ -z "$open_paths" ]; then
    echo "reclaim: WARNING: lsof returned no open build paths at all." >&2
    echo "reclaim: that is unlikely on a busy machine -- treating every cache as LIVE." >&2
    echo "reclaim: re-run when lsof works rather than deleting blind." >&2
    exit 1
fi

now="$(date +%s)"
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

    branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo DETACHED)"
    dirty="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    # The queue rebases, so a landed branch is never an ancestor of master --
    # patch-equivalence is the only honest test.
    unlanded="$(git -C "$wt" cherry master HEAD 2>/dev/null | grep -c '^+' || true)"
    unlanded="${unlanded//[!0-9]/}"; unlanded="${unlanded:-0}"

    for t in "$wt"/target*; do
        [ -d "$t" ] || continue
        base="$(basename "$t")"
        state=""
        if grep -qF -- "$t" <<<"$open_paths"; then
            state=LIVE
        elif [ "$base" = target ] && [ "$include_clones" -eq 0 ]; then
            state=CLONE
        elif [ "$(( (now - $(stat -f %m "$t")) / 3600 ))" -lt "$min_age_hours" ]; then
            state=RECENT
        elif [ "$unlanded" -gt 0 ] && [ "$include_unlanded" -eq 0 ]; then
            state=UNLANDED
        elif [ "$dirty" -gt 0 ] && [ "$include_unlanded" -eq 0 ]; then
            state=DIRTY
        else
            state=STALE
        fi
        size="$(du -sm "$t" 2>/dev/null | cut -f1 || echo 0)"
        printf '%9s  %-8s  %s\n' "$size" "$state" "${t#"$main"/}"
        if [ "$state" = STALE ]; then
            # Carry the mtime so the deletion order is oldest-first: the least
            # recently touched cache is the one whose owner is least likely to
            # be mid-task and pay for the rebuild.
            doomed+=("$(stat -f %m "$t")	$t")
            freed_candidates=$((freed_candidates + size))
        fi
    done
done < <(git -C "$main" worktree list --porcelain | awk '/^worktree /{print $2}')

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
        *) echo "reclaim: refusing to remove out-of-tree path: $t" >&2; exit 3 ;;
    esac
    rm -rf "$t"
    printf 'reclaim: removed %-58s  free now %s MB\n' "${t#"$main"/}" "$(free_mb)"
done < <(printf '%s\n' ${doomed[0]+"${doomed[@]}"} | sort -n)

end_free="$(free_mb)"
printf '\nreclaim: %s MB -> %s MB free (+%s MB actually reclaimed)\n' \
    "$start_free" "$end_free" "$((end_free - start_free))"
