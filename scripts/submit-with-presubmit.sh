#!/usr/bin/env bash
# Run the canonical focused edit checks and submit the exact clean tip.
#
# The ordinary merge-queue submit path remains advisory and unchanged. The
# normal wrapper names the checkout currently running the checks. `--detached`
# validates an unclaimed local branch from a clean shared master checkout in a
# temporary worktree, then submits its exact tip from the original checkout.
set -euo pipefail

cd "$(dirname "$0")/.."

current_branch="$(git branch --show-current 2>/dev/null || true)"
detached=0
if [ "${1:-}" = "--detached" ]; then
    detached=1
    shift
    branch="${1:-}"
    shift || true
else
    branch="${1:-$current_branch}"
fi
if [ -z "$branch" ] || [[ "$branch" == -* ]]; then
    echo "usage: ./scripts/submit-with-presubmit.sh [current-branch] [note]" >&2
    echo "       ./scripts/submit-with-presubmit.sh --detached BRANCH [note]" >&2
    exit 2
fi

if [ "$detached" -eq 0 ] && [ "$current_branch" != "$branch" ]; then
    echo "submit-with-presubmit: checkout branch '$branch' is not current branch '$current_branch'" >&2
    echo "submit-with-presubmit: run this from the branch worktree whose tip you intend to queue" >&2
    exit 2
fi

if [ "$detached" -eq 0 ]; then
    ./scripts/agent-check.sh edit
    if [ "$#" -eq 0 ]; then
        exec ./scripts/merge-queue.sh submit "$branch"
    fi
    exec ./scripts/merge-queue.sh submit "$@"
fi

if [ "$current_branch" != "master" ]; then
    echo "submit-with-presubmit: --detached requires a clean master checkout" >&2
    exit 2
fi
if [ -n "$(git status --porcelain=v1 --untracked-files=all --ignore-submodules)" ]; then
    echo "submit-with-presubmit: refusing --detached from a dirty checkout" >&2
    exit 2
fi
git show-ref --verify --quiet "refs/heads/$branch" || {
    echo "submit-with-presubmit: local branch '$branch' does not exist" >&2
    exit 2
}
if git worktree list --porcelain | awk -v wanted="refs/heads/$branch" '
    $1 == "branch" && $2 == wanted { found = 1 }
    END { exit found ? 0 : 1 }
'; then
    echo "submit-with-presubmit: branch '$branch' is already checked out; refusing to share its worktree" >&2
    exit 2
fi

tip_sha="$(git rev-parse "refs/heads/$branch")"
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/witchy-submit-with-presubmit.XXXXXX")"
temporary_worktree="$temporary_root/branch"
cleanup() {
    git worktree remove "$temporary_worktree" >/dev/null 2>&1 || true
    rmdir "$temporary_root" >/dev/null 2>&1 || true
}
trap cleanup EXIT
git worktree add --quiet "$temporary_worktree" "$branch"
(
    cd "$temporary_worktree"
    ./scripts/agent-check.sh edit
)
if [ "$(git rev-parse "refs/heads/$branch")" != "$tip_sha" ]; then
    echo "submit-with-presubmit: branch '$branch' moved during detached checks" >&2
    exit 1
fi
if [ "$#" -eq 0 ]; then
    exec ./scripts/merge-queue.sh submit "$branch"
fi
exec ./scripts/merge-queue.sh submit "$branch" "$@"
