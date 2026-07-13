#!/usr/bin/env bash
# Warm a fresh git worktree's build cache by cloning the main worktree's
# `target/` directory. macOS uses APFS copy-on-write (`cp -c`), GNU cp uses
# `--reflink=auto`, and other platforms fall back to an ordinary recursive copy.
#
# Why this works: dependency crates are compiled from ~/.cargo/registry paths,
# which don't vary by worktree, so their fingerprints stay valid — all ~270 dep
# crates (wasmtime included) come up warm. Only the 8 workspace crates
# fingerprint against the worktree path and rebuild. A cold multi-minute build
# becomes a workspace-only build.
#
# `incremental/` is deliberately NOT cloned: it holds only workspace-crate state
# (deps don't compile incrementally), which is path-keyed and thus dead weight in
# another worktree. Cargo's `split-debuginfo = "unpacked"` also leaves one
# `*.rcgu.o` file per codegen unit in `deps/`; those unpacked objects are not
# needed to reuse Cargo's fingerprints, libraries, or linked test binaries. The
# two sets dominate the file count (hundreds of thousands of tiny files), which
# is what makes a naive `cp -Rc target` take minutes instead of seconds.
#
#   ./scripts/worktree-warm.sh                     # warm the current worktree
#   ./scripts/worktree-warm.sh <path>              # warm the worktree at <path>
#   ./scripts/worktree-warm.sh --target-dir <dir>  # seed a per-agent
#                                                  # CARGO_TARGET_DIR (e.g.
#                                                  # target-codex) from ./target
#
# The --target-dir form is for agents sharing the MAIN checkout with an
# isolated target dir (the CLAUDE.md concurrency rule). Because the workspace
# path is identical there, even the workspace-crate fingerprints stay valid —
# the seeded dir is warmer than a worktree seed.
#
# Do NOT share one CARGO_TARGET_DIR across worktrees instead: cargo's build lock
# would serialize concurrent agents, and the local gates assume ./target
# (BUG-020). Per-worktree target + CoW seed keeps builds parallel AND warm.
set -euo pipefail

# This override is a portability escape hatch and a deterministic test hook.
copy_mode="${WITCHY_WORKTREE_WARM_COPY_MODE:-}"
if [[ -z "$copy_mode" ]]; then
    if cp --help 2>&1 | grep -F -- '--reflink' >/dev/null; then
        copy_mode="reflink"
    elif [[ "$(uname -s)" == "Darwin" ]]; then
        copy_mode="apfs"
    else
        copy_mode="copy"
    fi
fi
case "$copy_mode" in
    apfs) copy_description="APFS CoW clone" ;;
    reflink) copy_description="reflink/copy" ;;
    copy) copy_description="copy" ;;
    *)
        echo "worktree-warm: invalid WITCHY_WORKTREE_WARM_COPY_MODE '$copy_mode' (expected apfs, reflink, or copy)" >&2
        exit 2
        ;;
esac

copy_path() {
    case "$copy_mode" in
        apfs) cp -Rc "$@" ;;
        reflink) cp -R --reflink=auto "$@" ;;
        copy) cp -R "$@" ;;
    esac
}

clone_deps() { # clone_deps <src-deps> <dest-deps>
    local src="$1" dst="$2"
    mkdir "$dst"
    # Batch retained entries into as few cp invocations as the platform's
    # argument limit permits. A cp per file would erase most of the speedup.
    find "$src" -mindepth 1 -maxdepth 1 ! -name '*.rcgu.o' \
        -exec sh -c '
            mode="$1"; dst="$2"; shift 2
            case "$mode" in
                apfs) cp -Rc "$@" "$dst/" ;;
                reflink) cp -R --reflink=auto "$@" "$dst/" ;;
                copy) cp -R "$@" "$dst/" ;;
            esac
        ' sh "$copy_mode" "$dst" {} +
}

clone_profile() { # clone_profile <src-profile> <dest-profile>
    local src="${1%/}/" dst="$2" entry base
    mkdir "$dst"
    for entry in "$src".??* "$src"*; do
        base="$(basename "$entry")"
        [[ -e "$entry" ]] || continue
        [[ "$base" == "incremental" || "$base" == "." || "$base" == ".." ]] && continue
        if [[ "$base" == "deps" && -d "$entry" ]]; then
            clone_deps "$entry" "$dst/$base"
        else
            copy_path "$entry" "$dst/$base"
        fi
    done
}

clone_target() { # clone_target <src-target> <dest-target>
    local src="$1" dst="$2"
    mkdir "$dst"
    # CACHEDIR.TAG (written by cargo) marks target/ for backup tools to skip.
    [[ -f "$src/CACHEDIR.TAG" ]] && copy_path "$src/CACHEDIR.TAG" "$dst/" 2>/dev/null || true
    # Clone each profile dir. Target-triple dirs contain another debug/release
    # layer, so descend once to apply the same incremental/deps filtering.
    # copy_path uses the best clone primitive detected above, with an ordinary
    # recursive copy as the portable fallback.
    local profile name entry base
    for profile in "$src"/*/; do
        name="$(basename "$profile")"
        [[ "$name" == "tmp" ]] && continue
        if [[ -d "$profile/debug" || -d "$profile/release" ]]; then
            mkdir "$dst/$name"
            for entry in "$profile".??* "$profile"*; do
                base="$(basename "$entry")"
                [[ -e "$entry" ]] || continue
                [[ "$base" == "." || "$base" == ".." ]] && continue
                if [[ -d "$entry" && ( "$base" == "debug" || "$base" == "release" ) ]]; then
                    clone_profile "$entry" "$dst/$name/$base"
                else
                    copy_path "$entry" "$dst/$name/$base"
                fi
            done
        else
            clone_profile "$profile" "$dst/$name"
        fi
    done
}

if [[ "${1:-}" == "--target-dir" ]]; then
    dest="${2:?usage: worktree-warm.sh --target-dir <dir>}"
    root="$(git rev-parse --show-toplevel)"
    case "$dest" in /*) ;; *) dest="$root/$dest" ;; esac
    if [[ -e "$dest" ]]; then
        echo "worktree-warm: $dest already exists; leaving it alone" >&2
        exit 0
    fi
    if [[ ! -d "$root/target" ]]; then
        echo "worktree-warm: no $root/target to clone (run a build first)" >&2
        exit 1
    fi
    clone_target "$root/target" "$dest"
    echo "worktree-warm: seeded $dest from $root/target ($copy_description, incremental/ and deps/*.rcgu.o skipped)"
    exit 0
fi

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

clone_target "$main/target" "$dest/target"
echo "worktree-warm: seeded $dest/target from $main/target ($copy_description, incremental/ and deps/*.rcgu.o skipped)"
