#!/usr/bin/env bash
# Claude Code `WorktreeCreate` hook: when configured, this REPLACES the built-in
# `git worktree add` — the harness sends `{"hook_event_name":"WorktreeCreate",
# "name":"<worktree-name>"}` on stdin and expects the created worktree's
# absolute path as the ONLY stdout output (everything else must go to stderr;
# an empty stdout fails agent spawning with "returned no worktree path").
#
# This creator mimics the built-in layout (.claude/worktrees/<name>, branch
# worktree-<name> from HEAD) and then prepares its caches via worktree-warm.sh.
# The default target stays absent for Cargo 1.100 + mbx managed seeding; verdict
# caches are copied separately (see CLAUDE.md). Preparation is best-effort: a
# failure must never fail worktree creation.
set -u

root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"
cd "$root" || exit 1
script_dir="$(cd "$(dirname "$0")" && pwd)"
. "$script_dir/state-paths.sh"

# `--branch <branch>` names the branch exactly, for agents that create their
# own worktrees (Codex tasks create one per task: 148 rfc0167-* worktrees on
# 2026-09-25, each starting with no incremental state). The worktree name
# defaults to the branch with `/` replaced. An unavailable branch is an error
# rather than a renamed one, since the caller will submit that name.
explicit_branch=""
if [ "${1:-}" = "--branch" ]; then
    explicit_branch="${2:?worktree-create: --branch needs a branch name}"
    shift 2
    set -- "${1:-$(printf '%s' "$explicit_branch" | tr '/' '-')}"
fi

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
case "$name" in
    pool-[0-9][0-9])
        echo "worktree-create: '$name' is reserved for the worktree pool" >&2
        exit 1 ;;
esac

# A deleted branch can still be journaled as merged. Reusing that name would
# make the coordinator's next sweep remove this new, clean worktree.
branch_was_merged() {
    [ -f "$journal" ] &&
        jq -e --arg branch "$1" 'select(.event == "merged" and .branch == $branch)' "$journal" >/dev/null 2>&1
}
branch_unavailable() {
    git show-ref --verify --quiet "refs/heads/$1" || branch_was_merged "$1"
}

if [ -n "$explicit_branch" ]; then
    branch="$explicit_branch"
    if branch_unavailable "$branch"; then
        echo "worktree-create: branch $branch already exists or was previously merged" >&2
        exit 1
    fi
elif branch_unavailable "$branch"; then
    nonce="$(date +%s)-$$"
    branch="$base_branch-$nonce"
    attempt=0
    while branch_unavailable "$branch"; do
        attempt=$((attempt + 1))
        branch="$base_branch-$nonce-$attempt"
    done
    echo "worktree-create: branch $base_branch already exists or was previously merged; using $branch" >&2
fi

# RFC-0171: prefer a recycled pool worktree. Its path is one an earlier task
# already built in, so its managed target and rustc's path-keyed incremental
# state survive. Under the cap, a new worktree is created AT a pool path so the
# sweep can recycle it later.
# WITCHY_WORKTREE_POOL_MAX=0 turns the pool off.
. "$script_dir/worktree-pool.sh"
base="$(git rev-parse HEAD)"
if [ "$(worktree_pool_max)" -gt 0 ]; then
    if pooled="$(worktree_pool_claim "$root" "$branch" "$base")"; then
        echo "worktree-create: recycled pool worktree $pooled on $branch" >&2
        echo "$pooled"
        exit 0
    fi
    if slot="$(worktree_pool_reserve_new "$root")"; then
        dest="$(worktree_pool_slot_path "$root" "$slot")"
        trap 'worktree_pool_unreserve "$root" "$slot"' EXIT
    fi
fi

git worktree add -b "$branch" "$dest" "$base" 1>&2 || exit 1

"$root/scripts/worktree-warm.sh" "$dest" 1>&2 || true

# Optionally precompile workspace crates in the background. Cargo 1.100 target
# seeding supplies dependencies and mbx restores unchanged path crates, so this
# is normally unnecessary; the opt-in remains for callers that know they will
# consume both full profiles immediately.
# Use the new worktree's repository cargo shim: it preserves the unified feature
# graph, shares MBX artifacts, and participates in host-wide build admission.
# The per-target-dir Cargo lock still makes a race with the agent safe. Fully
# detached; never fails creation.
# Build both dev (for the binary) and test (for nextest) profiles — the test
# profile needs cfg(test) so it recompiles all workspace crates; doing both
# here means the agent's first `cargo nextest run` is a no-op link.
# A speculative pair of full-workspace builds for every created worktree
# overwhelmed this 10-core host
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
