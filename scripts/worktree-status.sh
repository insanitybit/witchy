#!/usr/bin/env bash
# One-screen dashboard of every worktree + branch state for concurrent agents:
# what exists, what's dirty, what's ahead/behind master, what's queued in the
# merge queue, and what looks abandoned.
#
#   ./scripts/worktree-status.sh            # the dashboard (reports only)
#   ./scripts/worktree-status.sh --disk     # include target/ disk usage (slow)
#   ./scripts/worktree-status.sh --equivalent # classify rebased patches (slow)
#   ./scripts/worktree-status.sh --prune    # remove fully-merged CLEAN worktrees
#   ./scripts/worktree-status.sh --branches # also delete merged local branches
set -euo pipefail

prune=0
prune_branches=0
show_disk=0
show_equivalent=0
for arg in "$@"; do
    case "$arg" in
        --disk) show_disk=1 ;;
        --equivalent) show_equivalent=1 ;;
        --prune) prune=1 ;;
        --branches) prune_branches=1 ;;
        -h|--help) sed -n '2,/^set -euo pipefail$/{ /^set -euo pipefail$/d; s/^# \{0,1\}//; p; }' "$0"; exit 0 ;;
        *) echo "worktree-status: unknown arg '$arg'" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | sed -n '1p' | sed 's/^worktree //')"
. "$here/scripts/state-paths.sh"
merge_queue_state="$(witchy_merge_queue_state_dir "$root")"
queue_dir="$merge_queue_state/queue"
journal="$merge_queue_state/journal.jsonl"
master_sha="$(git -C "$root" rev-parse master)"

queued_branches=""
if [ -d "$queue_dir" ]; then
    queued_branches="$(cat "$queue_dir"/*.json 2>/dev/null | jq -r .branch || true)"
fi
is_queued() { [ -n "$queued_branches" ] && grep -Fqx -- "$1" <<<"$queued_branches"; }

merged_branches=""
if [ -f "$journal" ]; then
    merged_branches="$(jq -r 'select(.event == "merged") | .branch' "$journal" | sort -u)"
fi
was_merged() { [ -n "$merged_branches" ] && grep -Fqx -- "$1" <<<"$merged_branches"; }

# The coordinator may rebase a queued commit before landing it, leaving the
# submitter's branch non-ancestral even though every patch is present on master.
# Reject branch-only merge commits because `git cherry` does not describe their
# conflict-resolution diff. Also require at least one result: an empty result can
# mean a merge-only history, which has no patch-id proof.
patch_equivalent_to_master() {
    local ref="$1" cherry
    [ -z "$(git -C "$root" rev-list --merges "master..$ref" 2>/dev/null || true)" ] || return 1
    cherry="$(git -C "$root" cherry master "$ref" 2>/dev/null || true)"
    [ -n "$cherry" ] && ! grep -q '^+' <<<"$cherry"
}

# Newer Git can compute every branch's ahead/behind counts in one ref walk.
# Keep a fallback for older installations rather than making the dashboard's
# reporting path depend on a particular Git release.
branch_inventory() {
    if git -C "$root" for-each-ref --count=1 --format='%(ahead-behind:master)' refs/heads >/dev/null 2>&1; then
        git -C "$root" for-each-ref \
            --format='%(refname:short)%09%(objectname)%09%(ahead-behind:master)%09%(committerdate:relative)' \
            refs/heads
    else
        git -C "$root" for-each-ref \
            --format='%(refname:short)%09%(objectname)%09%(committerdate:relative)' refs/heads |
            while IFS=$'\t' read -r branch sha last; do
                ahead="$(git -C "$root" rev-list --count "master..$branch")"
                behind="$(git -C "$root" rev-list --count "$branch..master")"
                printf '%s\t%s\t%s %s\t%s\n' "$branch" "$sha" "$ahead" "$behind" "$last"
            done
    fi
}

printf '%s\n' "worktrees (master @ ${master_sha:0:9}):"
git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2}' | while IFS= read -r wt; do
    if ! git -C "$wt" rev-parse --git-dir >/dev/null 2>&1; then
        printf '  %s\n    PRUNABLE metadata — worktree path or gitdir is missing\n' "$wt"
        continue
    fi
    branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
    dirty="clean"
    n="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    [ "$n" -gt 0 ] && dirty="DIRTY($n files)"
    counts="$(git -C "$wt" rev-list --left-right --count "master...HEAD" 2>/dev/null || echo '? ?')"
    behind="${counts%%[[:space:]]*}"
    ahead="${counts##*[[:space:]]}"
    last="$(git -C "$wt" log -1 --format='%h %ar %s' 2>/dev/null | cut -c1-70)"
    disk=""
    if [ "$show_disk" -eq 1 ] && [ -d "$wt/target" ]; then
        disk_mb="$(du -sm "$wt/target" 2>/dev/null | awk '{print $1}')"
        [ -n "$disk_mb" ] && disk=" [target: ${disk_mb}MB]"
    fi
    q=""
    is_queued "$branch" && q=" [QUEUED]"
    merged=""
    equivalent=0
    if [ "$ahead" != "0" ] && { [ "$show_equivalent" -eq 1 ] || [ "$prune" -eq 1 ]; }; then
        head_sha="$(git -C "$wt" rev-parse HEAD 2>/dev/null || echo '?')"
        if patch_equivalent_to_master "$head_sha"; then
            equivalent=1
            merged=" [patch-equivalent to master]"
        fi
    fi
    if [ "$wt" != "$root" ] && [ -z "$q" ] && was_merged "$branch"; then
        if [ "$ahead" = "0" ]; then
            merged=" [fully merged — removable]"
        elif [ "$equivalent" -eq 1 ]; then
            merged=" [patch-equivalent merge — removable]"
        fi
    fi
    printf '  %s\n    branch %s%s%s · %s · +%s/-%s vs master%s\n    last: %s\n' \
        "$wt" "$branch" "$q" "$merged" "$dirty" "$ahead" "$behind" "$disk" "$last"
    if [ -n "$merged" ] && [ "$dirty" = "clean" ]; then
        printf '    cleanup: git worktree remove %q\n' "$wt"
    fi
done

if [ "$prune" -eq 1 ]; then
    echo
    echo "pruning fully-merged clean worktrees:"
    git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2}' | while IFS= read -r wt; do
        [ "$wt" = "$root" ] && continue
        if ! git -C "$wt" rev-parse --git-dir >/dev/null 2>&1; then
            printf '  SKIP %s (prunable metadata; run git worktree prune explicitly)\n' "$wt"
            continue
        fi
        branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
        counts="$(git -C "$wt" rev-list --left-right --count "master...HEAD" 2>/dev/null || echo '? ?')"
        ahead="${counts##*[[:space:]]}"
        if [ "$ahead" != "0" ]; then
            head_sha="$(git -C "$wt" rev-parse HEAD 2>/dev/null || echo '?')"
            patch_equivalent_to_master "$head_sha" || continue
        fi
        is_queued "$branch" && { printf '  SKIP %s (%s is queued)\n' "$wt" "$branch"; continue; }
        was_merged "$branch" || { printf '  SKIP %s (%s is not journaled merged)\n' "$wt" "$branch"; continue; }
        n="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
        [ "$n" -eq 0 ] || { printf '  SKIP %s (dirty, %s files)\n' "$wt" "$n"; continue; }
        printf '  REMOVE %s (%s)\n' "$wt" "$branch"
        git -C "$root" worktree remove "$wt" 2>&1 | sed 's/^/    /'
    done
fi

echo
echo "local branches not checked out anywhere (candidates for deletion if merged):"
checked_out="$(git -C "$root" worktree list --porcelain | awk '/^branch /{sub("refs/heads/",""); print $2}')"
branch_inventory | while IFS=$'\t' read -r b branch_sha counts last; do
    [ "$b" = "master" ] && continue
    echo "$checked_out" | grep -Fqx -- "$b" && continue
    ahead="${counts%% *}"
    q=""
    is_queued "$b" && q=" [QUEUED]"
    if [ "$ahead" = "0" ]; then
        printf '  %-45s merged%s — cleanup: git update-ref -d %q %q\n' \
            "$b" "$q" "refs/heads/$b" "$branch_sha"
        if [ "$prune_branches" -eq 1 ] && [ -z "$q" ]; then
            if git -C "$root" merge-base --is-ancestor "$branch_sha" master &&
                git -C "$root" update-ref -d "refs/heads/$b" "$branch_sha"; then
                printf '    deleted %s at %s\n' "$b" "${branch_sha:0:9}"
            else
                printf '    SKIP %s (moved or no longer merged)\n' "$b"
            fi
        fi
    elif [ "$show_equivalent" -eq 1 ] && patch_equivalent_to_master "$branch_sha"; then
        printf '  %-45s %s commits patch-equivalent to master%s (retained; not ancestry-merged, last activity %s)\n' \
            "$b" "$ahead" "$q" "$last"
    else
        printf '  %-45s %s commits ahead%s (last activity %s)\n' "$b" "$ahead" "$q" "$last"
    fi
done
