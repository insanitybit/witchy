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
#   ./scripts/worktree-warm.sh --help               # show this help
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

gate_seed_source() { # gate_seed_source <main-worktree>
    local main="$1" qdir selected="target"
    [[ "${WITCHY_WORKTREE_WARM_GATE_LOCKED:-}" == 1 ]] || return 1
    [[ -f "$main/scripts/state-paths.sh" ]] || return 1
    # shellcheck source=state-paths.sh
    source "$main/scripts/state-paths.sh"
    qdir="$(witchy_merge_queue_state_dir "$main")" || return 1
    if [[ -f "$qdir/gate-target" ]]; then
        selected="$(cat "$qdir/gate-target" 2>/dev/null || true)"
    fi
    case "$selected" in target | target-prewarm) ;; *) return 1 ;; esac
    [[ -d "$main/.claude/worktrees/merge-gate/$selected" ]] || return 1
    printf '%s\n' "$main/.claude/worktrees/merge-gate/$selected"
}

clone_or_gate_fallback() { # clone_or_gate_fallback <preferred-source> <dest> <main> <original-args...>
    local source="$1" dest="$2" main="$3"
    shift 3
    if [[ ! -d "$source" ]]; then
        if [[ "${WITCHY_WORKTREE_WARM_GATE_LOCKED:-}" == 1 ]]; then
            source="$(gate_seed_source "$main")" || {
                echo "worktree-warm: no safe warm target to clone" >&2
                return 1
            }
            # The persistent stdlib cache is keyed by the test executable's
            # identity. A gate-worktree binary is relinked in this checkout,
            # so copying its thousands of cache entries only adds metadata work.
            clone_testcache=0
        elif [[ -x "$main/scripts/merge-queue.sh" ]]; then
            local script
            script="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
            exec env WITCHY_WORKTREE_WARM_GATE_LOCKED=1 \
                WITCHY_WORKTREE_WARM_COPY_MODE="$copy_mode" \
                "$main/scripts/merge-queue.sh" with-lock -- "$script" "$@"
        else
            echo "worktree-warm: no $source to clone (run a build in the main worktree first)" >&2
            return 1
        fi
    fi
    clone_target "$source" "$dest"
    local skipped="incremental/ and deps/*.rcgu.o skipped"
    [[ "$clone_testcache" == 1 ]] || skipped="$skipped, stale witchy-testcache skipped"
    echo "worktree-warm: seeded $dest from $source ($copy_description, $skipped)"
}

# The build seed (scripts/build-seed.py) is the gate's target for the latest
# landed master plus the gate checkout's file mtimes. Applying those mtimes is
# what lets cargo judge the workspace crates Fresh: a fresh checkout stamps
# every file with the current time, which alone rebuilt all of them. The main
# checkout's target/ is the fallback, but nobody builds there, so it trails
# master by days. Returns 1 (caller falls back) when there is no usable seed.
seed_from_build_seed() { # seed_from_build_seed <main> <dest-worktree>
    local main="$1" dest="$2" seed
    [[ "${WITCHY_BUILD_SEED:-on}" == off ]] && return 1
    seed="$(cd "$main/state/build-seed/current" 2>/dev/null && pwd -P)" || return 1
    [[ -f "$seed/manifest.json" && -d "$seed/target" ]] || return 1
    clone_testcache=0
    clone_target "$seed/target" "$dest/target" || { rm -rf "$dest/target"; return 1; }
    echo "worktree-warm: seeded $dest/target from $seed ($copy_description, incremental/ and deps/*.rcgu.o skipped)"
    python3 "$(dirname "$0")/build-seed.py" apply "$seed" "$dest" \
        || echo "worktree-warm: could not apply seed mtimes; workspace crates will rebuild" >&2
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
    clone_or_gate_fallback "$root/target" "$dest" "$main" "$@"
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
    exit 0
fi
clone_testcache=0
seed_from_build_seed "$main" "$dest" \
    || clone_or_gate_fallback "$main/target" "$dest/target" "$main" "$@"
seed_verdict_cache_from_best_source "$main" "$dest"
