#!/usr/bin/env bash
# Warm a fresh git worktree's build cache by APFS-cloning the main worktree's
# `target/` directory (copy-on-write via `cp -c`: no real disk used until files
# diverge).
#
# Why this works: dependency crates are compiled from ~/.cargo/registry paths,
# which don't vary by worktree, so their fingerprints stay valid — all ~270 dep
# crates (wasmtime included) come up warm. Only the 8 workspace crates
# fingerprint against the worktree path and rebuild. A cold multi-minute build
# becomes a workspace-only build.
#
# `incremental/` is deliberately NOT cloned: it holds only workspace-crate state
# (deps don't compile incrementally), which is path-keyed and thus dead weight in
# another worktree — and it dominates the file count (hundreds of thousands of
# tiny files), which is what makes a naive `cp -Rc target` take minutes instead
# of seconds.
#
#   ./scripts/worktree-warm.sh            # warm the current worktree
#   ./scripts/worktree-warm.sh <path>     # warm the worktree at <path>
#
# Do NOT share one CARGO_TARGET_DIR across worktrees instead: cargo's build lock
# would serialize concurrent agents, and the local gates assume ./target
# (BUG-020). Per-worktree target + CoW seed keeps builds parallel AND warm.
set -euo pipefail

dest="${1:-$(pwd)}"
dest="$(cd "$dest" && pwd)"

# The main worktree is the first entry in `git worktree list`.
main="$(git -C "$dest" worktree list --porcelain | head -1 | sed 's/^worktree //')"

if [[ "$dest" == "$main" ]]; then
    echo "worktree-warm: $dest is the main worktree; nothing to seed" >&2
    exit 0
fi
if [[ -e "$dest/target" ]]; then
    echo "worktree-warm: $dest/target already exists; leaving it alone" >&2
    exit 0
fi
if [[ ! -d "$main/target" ]]; then
    echo "worktree-warm: no $main/target to clone (run a build in the main worktree first)" >&2
    exit 1
fi

mkdir "$dest/target"
# CACHEDIR.TAG (written by cargo) marks target/ for backup tools to skip.
[[ -f "$main/target/CACHEDIR.TAG" ]] && cp -c "$main/target/CACHEDIR.TAG" "$dest/target/" 2>/dev/null || true

# Clone each profile dir (debug, release, per-target triples), skipping
# incremental/. `cp -Rc` requests a CoW clone (APFS); on a non-CoW filesystem it
# falls back to a real copy — still correct, just slower.
for profile in "$main"/target/*/; do
    name="$(basename "$profile")"
    [[ "$name" == "tmp" ]] && continue
    mkdir "$dest/target/$name"
    for entry in "$profile".??* "$profile"*; do
        base="$(basename "$entry")"
        [[ -e "$entry" ]] || continue
        [[ "$base" == "incremental" || "$base" == "." || "$base" == ".." ]] && continue
        cp -Rc "$entry" "$dest/target/$name/$base"
    done
done
echo "worktree-warm: seeded $dest/target from $main/target (CoW clone, incremental/ skipped)"
