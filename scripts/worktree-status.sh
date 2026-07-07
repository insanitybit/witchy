#!/usr/bin/env bash
# One-screen dashboard of every worktree + branch state for concurrent agents:
# what exists, what's dirty, what's ahead/behind master, what's queued in the
# merge queue, and what looks abandoned. REPORTS ONLY — suggests cleanup
# commands, never runs them.
#
#   ./scripts/worktree-status.sh          # the dashboard
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | head -1 | sed 's/^worktree //')"
queue_dir="$root/scratch/merge-queue/queue"
master_sha="$(git -C "$root" rev-parse master)"

queued_branches=""
if [ -d "$queue_dir" ]; then
    queued_branches="$(cat "$queue_dir"/*.json 2>/dev/null | jq -r .branch | paste -sd'|' - || true)"
fi
is_queued() { [ -n "$queued_branches" ] && echo "$1" | grep -qxE "$queued_branches"; }

printf '%s\n' "worktrees (master @ ${master_sha:0:9}):"
git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2}' | while IFS= read -r wt; do
    branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
    dirty="clean"
    n="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
    [ "$n" -gt 0 ] && dirty="DIRTY($n files)"
    ahead="$(git -C "$wt" rev-list --count "master..HEAD" 2>/dev/null || echo '?')"
    behind="$(git -C "$wt" rev-list --count "HEAD..master" 2>/dev/null || echo '?')"
    last="$(git -C "$wt" log -1 --format='%h %ar %s' 2>/dev/null | cut -c1-70)"
    q=""
    is_queued "$branch" && q=" [QUEUED]"
    merged=""
    if [ "$ahead" = "0" ] && [ "$wt" != "$root" ]; then merged=" [fully merged — removable]"; fi
    printf '  %s\n    branch %s%s%s · %s · +%s/-%s vs master\n    last: %s\n' \
        "$wt" "$branch" "$q" "$merged" "$dirty" "$ahead" "$behind" "$last"
    if [ -n "$merged" ] && [ "$dirty" = "clean" ]; then
        printf '    cleanup: git worktree remove %q\n' "$wt"
    fi
done

echo
echo "local branches not checked out anywhere (candidates for deletion if merged):"
checked_out="$(git -C "$root" worktree list --porcelain | awk '/^branch /{sub("refs/heads/",""); print $2}')"
git -C "$root" for-each-ref refs/heads --format='%(refname:short)' | while IFS= read -r b; do
    echo "$checked_out" | grep -qx "$b" && continue
    ahead="$(git -C "$root" rev-list --count "master..$b")"
    q=""
    is_queued "$b" && q=" [QUEUED]"
    if [ "$ahead" = "0" ]; then
        printf '  %-45s merged%s — cleanup: git branch -d %q\n' "$b" "$q" "$b"
    else
        last="$(git -C "$root" log -1 --format='%ar' "$b")"
        printf '  %-45s %s commits ahead%s (last activity %s)\n' "$b" "$ahead" "$q" "$last"
    fi
done
