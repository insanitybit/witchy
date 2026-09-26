#!/usr/bin/env bash
# Prepare a fresh git worktree for fast builds. Its default `target/` stays
# absent so Cargo 1.100 and mbx can create a managed target and copy dependency
# units directly from another checkout. Explicit target directories still use
# the filtered clone machinery below because Cargo excludes them from managed
# placement.
#
# Cargo 1.100 target seeding copies registry and Git units while leaving path
# packages to the action cache and learned incremental state. That avoids a
# per-unit cache restore and prevents path-bound native build scripts such as
# aws-lc-sys from compiling once per checkout.
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
#   ./scripts/worktree-warm.sh --clone-target <source> <dest>
#                                                  # clone one Cargo generation
#                                                  # into another (coordinator)
#   ./scripts/worktree-warm.sh --help               # show this help
#
# The --target-dir form clones only from the same checkout's default target.
# Cross-checkout path units are never copied: Cargo 1.100 stores them in
# checkout-bound per-unit directories, so mbx's action cache is the safe
# fallback when the local default target does not exist.
#
# Do NOT share one CARGO_TARGET_DIR across worktrees instead: cargo's build lock
# would serialize concurrent agents, and the local gates assume ./target
# (BUG-020). Per-worktree target + CoW seed keeps builds parallel AND warm.
#
set -euo pipefail

print_help() {
    sed -n '2,/^set -euo pipefail$/{ /^set -euo pipefail$/d; s/^# \{0,1\}//; p; }' "$0"
}

case "${1:-}" in
    -h | --help)
        print_help
        exit 0
        ;;
    --target-dir)
        if [[ "$#" -ne 2 || -z "${2:-}" ]]; then
            echo "usage: worktree-warm.sh --target-dir <dir>" >&2
            exit 2
        fi
        ;;
    --clone-target)
        if [[ "$#" -ne 3 || -z "${2:-}" || -z "${3:-}" ]]; then
            echo "usage: worktree-warm.sh --clone-target <source> <dest>" >&2
            exit 2
        fi
        ;;
    -*)
        echo "worktree-warm: unknown option '${1}' (try --help)" >&2
        exit 2
        ;;
    *)
        if [[ "$#" -gt 1 ]]; then
            echo "usage: worktree-warm.sh [<worktree-path> | --target-dir <dir>]" >&2
            exit 2
        fi
        ;;
esac

# This override is a portability escape hatch and a deterministic test hook.
copy_mode="${WITCHY_WORKTREE_WARM_COPY_MODE:-}"
clone_testcache=1
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

# Every mode keeps mtimes (APFS clones do anyway): cargo compares them, and a
# seeded fingerprint stamped "now" would look newer than an edited source.
copy_path() {
    case "$copy_mode" in
        apfs) cp -Rc "$@" ;;
        reflink) cp -R --reflink=auto --preserve=timestamps "$@" ;;
        copy) cp -Rp "$@" ;;
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
            # The gate target can still be compiling while it is cloned. A
            # dependency object may disappear between find and cp; retain the
            # rest of the warm cache instead of failing the whole seed and
            # forcing the caller onto a cold target.
            case "$mode" in
                apfs) cp -Rc "$@" "$dst/" 2>/dev/null || true ;;
                reflink) cp -R --reflink=auto --preserve=timestamps "$@" "$dst/" 2>/dev/null || true ;;
                copy) cp -Rp "$@" "$dst/" 2>/dev/null || true ;;
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
        [[ "$clone_testcache" == 0 && "$name" == "witchy-testcache" ]] && continue
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

if [[ "${1:-}" == "--clone-target" ]]; then
    source_target="$2"
    dest_target="$3"
    case "$source_target" in /*) ;; *) source_target="$PWD/$source_target" ;; esac
    case "$dest_target" in /*) ;; *) dest_target="$PWD/$dest_target" ;; esac
    if [[ ! -d "$source_target" ]]; then
        echo "worktree-warm: no source target at $source_target" >&2
        exit 1
    fi
    if [[ -e "$dest_target" ]]; then
        echo "worktree-warm: destination already exists at $dest_target" >&2
        exit 1
    fi
    # Test-result caches are keyed by the linked executable identity. Cargo
    # will relink workspace tests in the new generation, so these entries are
    # metadata overhead rather than reusable build artifacts.
    clone_testcache=0
    clone_target "$source_target" "$dest_target"
    echo "worktree-warm: cloned $dest_target from $source_target ($copy_description, incremental/, deps/*.rcgu.o and stale witchy-testcache skipped)"
    exit 0
fi

# The checker's persistent verdict store (see
# crates/witchy-wir/src/canonical_safewir/check/cache.rs). A fresh worktree
# starts with an empty one, so every test process in it re-establishes the
# expensive standard-library verdicts. Measured 2026-09-20: one `witchy check`
# of an async program takes 12s cold and 5s with the store warm, because the
# async executor's verdict alone costs 5.9s to establish; a fresh worktree
# seeded from the gate's store replayed 2 verdicts on its first async test
# with 0 stores. The entries are keyed by a digest of the checker's own
# sources, so a changed checker never reads an older checker's verdicts, and
# each file carries an integrity digest — a stale or truncated entry is a
# miss, never a wrong verdict. Best effort: no store, or a failed copy, just
# means the worktree establishes them itself.
clone_verdict_cache() { # clone_verdict_cache <main> <dest>
    local src="$1/state/safewir-check-cache" dst="$2/state/safewir-check-cache"
    [[ -d "$src" ]] || return 0
    [[ -e "$dst" ]] && return 0
    mkdir -p "$2/state" 2>/dev/null || return 0
    copy_path "$src" "$dst" 2>/dev/null || return 0
    printf 'worktree-warm: seeded %s (%s verdicts)\n' "$dst" "$(ls "$dst" 2>/dev/null | wc -l | tr -d ' ')"
}

# The gate compiles master continuously, so its store is the richest one on
# the machine; fall back to the main checkout's.
seed_verdict_cache_from_best_source() { # <main> <dest-root>
    local main="$1" dest="$2" gate="$1/.claude/worktrees/merge-gate"
    if [[ -d "$gate/state/safewir-check-cache" ]]; then
        clone_verdict_cache "$gate" "$dest"
    else
        clone_verdict_cache "$main" "$dest"
    fi
}

if [[ "${1:-}" == "--target-dir" ]]; then
    dest="$2"
    root="$(git rev-parse --show-toplevel)"
    main="$(git worktree list --porcelain | sed -n '1p' | sed 's/^worktree //')"
    case "$dest" in /*) ;; *) dest="$root/$dest" ;; esac
    if [[ -e "$dest" ]]; then
        echo "worktree-warm: $dest already exists; leaving it alone" >&2
        exit 0
    fi
    if [[ -d "$root/target" ]]; then
        clone_testcache=0
        clone_target "$root/target" "$dest"
        echo "worktree-warm: seeded $dest from $root/target ($copy_description, same-checkout units; incremental/ and deps/*.rcgu.o skipped)"
    else
        echo "worktree-warm: no same-checkout target to clone; mbx action cache will warm $dest" >&2
    fi
    seed_verdict_cache_from_best_source "$main" "$root"
    exit $?
fi

dest="${1:-$(pwd)}"
dest="$(cd "$dest" && pwd)"

# The main worktree is the first entry in `git worktree list`.
main="$(git -C "$dest" worktree list --porcelain | sed -n '1p' | sed 's/^worktree //')"

if [[ "$dest" == "$main" ]]; then
    echo "worktree-warm: $dest is the main worktree; nothing to seed" >&2
    exit 0
fi
if [[ -e "$dest/target" ]]; then
    echo "worktree-warm: $dest/target already exists; leaving it alone" >&2
else
    echo "worktree-warm: leaving $dest/target to Cargo 1.100 + mbx managed seeding"
fi
seed_verdict_cache_from_best_source "$main" "$dest"
