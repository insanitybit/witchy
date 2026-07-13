#!/usr/bin/env bash
# One-screen dashboard of every worktree + branch state for concurrent agents:
# what exists, what's dirty, what's ahead/behind master, what's queued in the
# merge queue, and what looks abandoned.
#
#   ./scripts/worktree-status.sh            # the dashboard (reports only)
#   ./scripts/worktree-status.sh --prune    # remove fully-merged CLEAN worktrees
#   ./scripts/worktree-status.sh --branches # also delete merged local branches
set -euo pipefail

prune=0
prune_branches=0
for arg in "$@"; do
    case "$arg" in
        --prune) prune=1 ;;
        --branches) prune_branches=1 ;;
        -h|--help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "worktree-status: unknown arg '$arg'" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | head -1 | sed 's/^worktree //')"
queue_dir="$root/scratch/merge-queue/queue"
journal="$root/scratch/merge-queue/journal.jsonl"
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

printf '%s\n' "worktrees (master @ ${master_sha:0:9}):"
git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2}' | while IFS= read -r wt; do
    branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
    dirty="clean"
    n="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    [ "$n" -gt 0 ] && dirty="DIRTY($n files)"
    ahead="$(git -C "$wt" rev-list --count "master..HEAD" 2>/dev/null || echo '?')"
    behind="$(git -C "$wt" rev-list --count "HEAD..master" 2>/dev/null || echo '?')"
    last="$(git -C "$wt" log -1 --format='%h %ar %s' 2>/dev/null | cut -c1-70)"
    disk=""
    if [ -d "$wt/target" ]; then
        disk_mb="$(du -sm "$wt/target" 2>/dev/null | awk '{print $1}')"
        [ -n "$disk_mb" ] && disk=" [target: ${disk_mb}MB]"
    fi
    q=""
    is_queued "$branch" && q=" [QUEUED]"
    merged=""
    if [ "$ahead" = "0" ] && [ "$wt" != "$root" ] && [ -z "$q" ] && was_merged "$branch"; then
        merged=" [fully merged — removable]"
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
        branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
        ahead="$(git -C "$wt" rev-list --count "master..HEAD" 2>/dev/null || echo '?')"
        [ "$ahead" = "0" ] || continue
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
git -C "$root" for-each-ref refs/heads --format='%(refname:short)' | while IFS= read -r b; do
    [ "$b" = "master" ] && continue
    echo "$checked_out" | grep -qx "$b" && continue
    branch_sha="$(git -C "$root" rev-parse "$b")"
    ahead="$(git -C "$root" rev-list --count "master..$b")"
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
    else
        last="$(git -C "$root" log -1 --format='%ar' "$b")"
        printf '  %-45s %s commits ahead%s (last activity %s)\n' "$b" "$ahead" "$q" "$last"
    fi
done
