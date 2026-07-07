#!/usr/bin/env bash
# Claude Code `WorktreeCreate` hook: when configured, this REPLACES the built-in
# `git worktree add` — the harness sends `{"hook_event_name":"WorktreeCreate",
# "name":"<worktree-name>"}` on stdin and expects the created worktree's
# absolute path as the ONLY stdout output (everything else must go to stderr;
# an empty stdout fails agent spawning with "returned no worktree path").
#
# This creator mimics the built-in layout (.claude/worktrees/<name>, branch
# worktree-<name> from HEAD) and then CoW-seeds the new worktree's target/ via
# worktree-warm.sh so agents start with a warm build cache (see CLAUDE.md).
# Warming is best-effort: a warm failure must never fail worktree creation.
set -u

root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"
cd "$root" || exit 1

name=$(jq -r '.name // empty' 2>/dev/null || true)
[ -n "$name" ] || name="wt-$$-$(date +%s)"

dest="$root/.claude/worktrees/$name"
branch="worktree-$name"

if [ -e "$dest" ]; then
    echo "worktree-create: $dest already exists" >&2
    exit 1
fi

# Unique-ify the branch if a previous worktree left one behind.
if git show-ref --verify --quiet "refs/heads/$branch"; then
    branch="$branch-$(date +%s)"
fi

git worktree add -b "$branch" "$dest" HEAD 1>&2 || exit 1

"$root/scripts/worktree-warm.sh" "$dest" 1>&2 || true

# Precompile the workspace crates in the background: the CoW seed leaves deps
# warm but the 8 workspace crates cold (their fingerprints are path-keyed), so
# the agent's first build pays ~1-2 min. Starting it now means that cost runs
# during the agent's read/think phase instead of blocking its first command.
# Safe to race the agent's own cargo: the per-target-dir build lock serializes
# them and the work is shared either way. Fully detached; never fails creation.
if command -v cargo >/dev/null 2>&1; then
    nohup cargo build --workspace --manifest-path "$dest/Cargo.toml" \
        >"$dest/.worktree-prebuild.log" 2>&1 </dev/null &
    disown
    echo "worktree-create: background workspace prebuild started (log: $dest/.worktree-prebuild.log)" >&2
fi

echo "$dest"
