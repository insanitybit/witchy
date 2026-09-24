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
script_dir="$(cd "$(dirname "$0")" && pwd)"
. "$script_dir/state-paths.sh"

# Accept name from: (1) positional arg, (2) JSON on stdin (hook protocol), (3) generate one.
name="${1:-}"
if [ -z "$name" ] && [ ! -t 0 ]; then
    name=$(jq -r '.name // empty' 2>/dev/null || true)
fi
[ -n "$name" ] || name="wt-$$-$(date +%s)"

dest="$root/.claude/worktrees/$name"
base_branch="worktree-$name"
branch="$base_branch"
journal="$(witchy_merge_queue_state_dir "$root")/journal.jsonl"

if [ -e "$dest" ]; then
    echo "worktree-create: $dest already exists" >&2
    exit 1
fi

# A deleted branch can still be journaled as merged. Reusing that name would
# make the coordinator's next sweep remove this new, clean worktree.
branch_was_merged() {
    [ -f "$journal" ] &&
        jq -e --arg branch "$1" 'select(.event == "merged" and .branch == $branch)' "$journal" >/dev/null 2>&1
}
branch_unavailable() {
    git show-ref --verify --quiet "refs/heads/$1" || branch_was_merged "$1"
}

if branch_unavailable "$branch"; then
    nonce="$(date +%s)-$$"
    branch="$base_branch-$nonce"
    attempt=0
    while branch_unavailable "$branch"; do
        attempt=$((attempt + 1))
        branch="$base_branch-$nonce-$attempt"
    done
    echo "worktree-create: branch $base_branch already exists or was previously merged; using $branch" >&2
fi

git worktree add -b "$branch" "$dest" HEAD 1>&2 || exit 1

"$root/scripts/worktree-warm.sh" "$dest" 1>&2 || true

# Precompile the workspace crates in the background: the CoW seed leaves deps
# warm but the 8 workspace crates cold (their fingerprints are path-keyed), so
# the agent's first build pays ~1-2 min. Starting it now means that cost runs
# during the agent's read/think phase instead of blocking its first command.
# Use the new worktree's repository cargo shim: it preserves the unified feature
# graph, shares MBX artifacts, and participates in host-wide build admission.
# The per-target-dir Cargo lock still makes a race with the agent safe. Fully
# detached; never fails creation.
# Build both dev (for the binary) and test (for nextest) profiles — the test
# profile needs cfg(test) so it recompiles all workspace crates; doing both
# here means the agent's first `cargo nextest run` is a no-op link.
# The warm seed makes a fresh target immediately usable. A speculative pair of
# full-workspace builds for every created worktree overwhelmed this 10-core host
# when several agents started together. Opt in only when the caller knows it
# will consume both profiles.
if [ "${WITCHY_WORKTREE_CREATE_PREBUILD:-0}" = "1" ] && command -v cargo >/dev/null 2>&1; then
    priority=()
    if command -v taskpolicy >/dev/null 2>&1; then
        priority=(taskpolicy -c utility)
    elif command -v nice >/dev/null 2>&1; then
        priority=(nice -n 10)
    fi
    prebuild_cargo="cargo"
    [ ! -x "$dest/scripts/bin/cargo" ] || prebuild_cargo="$dest/scripts/bin/cargo"
    nohup "${priority[@]}" sh -c '
        cargo_runner="$1"
        manifest="$2"
        "$cargo_runner" build --workspace --manifest-path "$manifest" &&
            "$cargo_runner" test --workspace --no-run --manifest-path "$manifest"
    ' sh "$prebuild_cargo" "$dest/Cargo.toml" \
        >"$dest/.worktree-prebuild.log" 2>&1 </dev/null &
    disown
    echo "worktree-create: background workspace prebuild started (log: $dest/.worktree-prebuild.log)" >&2
fi

echo "$dest"
