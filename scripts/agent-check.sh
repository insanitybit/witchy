#!/usr/bin/env bash
# Focused validation entrypoint for agents.
# This script intentionally runs targeted checks, never the full workspace gate.
#
#   ./scripts/agent-check.sh target --package <name> [--filter <filter>]
#   ./scripts/agent-check.sh paths <path-pattern...>
#   ./scripts/agent-check.sh syntax
#   ./scripts/agent-check.sh link
#   ./scripts/agent-check.sh parity
set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
    sed -n '2,80p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

run_in_agent_env() {
    env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR="$AGENT_TARGET_DIR" "$@"
}

run_paths_checks() {
    ./scripts/test-for-paths.sh --run "$@"
}

ensure_agent_target() {
    [ -d "$AGENT_TARGET_DIR" ] || [ ! -d target ] || \
        ./scripts/worktree-warm.sh --target-dir "$AGENT_TARGET_DIR"
}

AGENT_TARGET_DIR="${CARGO_TARGET_DIR:-target-codex}"
command="${1:-}"
shift || true

if [ -z "$command" ]; then
    usage 2
fi

while [ "$#" -gt 0 ] && [ "${1:-}" = "--target-dir" ]; do
    if [ "${2:-}" = "" ]; then
        echo "agent-check: --target-dir requires a path argument" >&2
        exit 2
    fi
    AGENT_TARGET_DIR="$2"
    shift 2
done

case "$command" in
    target)
        package=""
        filter=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --package)
                    package="${2:-}"
                    shift 2
                    ;;
                --filter)
                    filter="${2:-}"
                    shift 2
                    ;;
                -h | --help)
                    usage 0
                    ;;
                *)
                    echo "agent-check: unexpected target argument '$1'" >&2
                    usage 2
                    ;;
            esac
        done
        if [ -z "$package" ]; then
            echo "agent-check: target requires --package <name>" >&2
            exit 2
        fi
        ensure_agent_target
        if cargo nextest --version >/dev/null 2>&1; then
            cmd=(cargo nextest run -p "$package")
            if [ -n "$filter" ]; then
                cmd+=(-E "test($filter)")
            fi
        else
            cmd=(cargo test -p "$package")
            if [ -n "$filter" ]; then
                cmd+=(-- "$filter")
            fi
        fi
        run_in_agent_env "${cmd[@]}"
        ;;

    paths)
        if [ "$#" -eq 0 ]; then
            echo "agent-check: paths command requires at least one pattern" >&2
            exit 2
        fi
        run_paths_checks "$@"
        ;;

    syntax)
        run_paths_checks crates/witchy-syntax/src crates/witchy-syntax/Cargo.toml
        ;;

    link)
        run_paths_checks crates/witchy-syntax/src crates/witchy-syntax/Cargo.toml \
            crates/witchy-types/src crates/witchy-types/Cargo.toml
        ;;

    parity)
        run_paths_checks \
            crates/witchy-syntax/src crates/witchy-syntax/Cargo.toml \
            crates/witchy-types/src crates/witchy-types/Cargo.toml \
            crates/witchy-interp/src crates/witchy-interp/Cargo.toml \
            crates/witchy-lower/src crates/witchy-lower/Cargo.toml \
            crates/witchy-runtime/src crates/witchy-runtime/Cargo.toml
        ;;

    -h | --help)
        usage 0
        ;;

    *)
        echo "agent-check: unknown command '$command'" >&2
        usage 2
        ;;
esac
