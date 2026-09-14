#!/usr/bin/env bash
# Run the canonical focused edit checks and submit the exact clean tip.
#
# This is an opt-in convenience wrapper. The ordinary merge-queue submit path
# remains advisory and unchanged; this wrapper is deliberately limited to a
# optional branch argument that names the checkout currently running the checks.
set -euo pipefail

cd "$(dirname "$0")/.."

current_branch="$(git branch --show-current 2>/dev/null || true)"
branch="${1:-$current_branch}"
if [ -z "$branch" ] || [[ "$branch" == -* ]]; then
    echo "usage: ./scripts/submit-with-presubmit.sh [current-branch] [note]" >&2
    exit 2
fi

if [ "$current_branch" != "$branch" ]; then
    echo "submit-with-presubmit: checkout branch '$branch' is not current branch '$current_branch'" >&2
    echo "submit-with-presubmit: run this from the branch worktree whose tip you intend to queue" >&2
    exit 2
fi

./scripts/agent-check.sh edit
if [ "$#" -eq 0 ]; then
    exec ./scripts/merge-queue.sh submit "$branch"
fi
exec ./scripts/merge-queue.sh submit "$@"
