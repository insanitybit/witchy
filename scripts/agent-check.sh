#!/usr/bin/env bash
# Focused validation entrypoint for agents.
# This script intentionally runs targeted checks, never the full workspace gate.
#
#   ./scripts/agent-check.sh target --package <name> [--filter <filter>]
#   ./scripts/agent-check.sh edit [path...]
#   ./scripts/agent-check.sh paths <path-pattern...>
#   ./scripts/agent-check.sh syntax
#   ./scripts/agent-check.sh link
#   ./scripts/agent-check.sh parity
#
# `edit` is the minimal inner-loop selection for the current diff (or explicit
# paths). The merge queue remains the separate landing set and runs its own
# serialized gate after submission.
#
# Set AGENT_CHECK_RUNNER=nextest to use nextest for a focused package; the
# default cargo test runner avoids nextest startup overhead for one binary.
set -euo pipefail

cd "$(dirname "$0")/.."

# QoS and target selection need the same repository identity. Enumerate linked
# worktrees once, retaining the existing non-Git fixture fallback.
agent_main_worktree="$(git worktree list --porcelain 2>/dev/null | sed -n '1s/^worktree //p' || true)"

usage() {
    sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit "${1:-0}"
}

gate_is_live() {
    local queue_state
    [ -n "$agent_main_worktree" ] || return 1
    # shellcheck source=state-paths.sh
    . ./scripts/state-paths.sh
    queue_state="$(witchy_merge_queue_state_dir "$agent_main_worktree")" || return 1
    [ -d "$queue_state/gate.lock" ]
}

# A live merge gate owns foreground CPU, so exact checks yield while its lock
# exists. At every other time they are latency-sensitive foreground work. The
# old unconditional utility QoS made healthy warm presubmits take hours when a
# loaded macOS host starved utility processes even though no gate was running.
agent_priority=(env)
if gate_is_live; then
    if command -v taskpolicy >/dev/null 2>&1; then
        agent_priority=(taskpolicy -c utility)
    elif command -v nice >/dev/null 2>&1; then
        agent_priority=(nice -n 10)
    fi
fi

# Focused validation is latency-sensitive. The repo cargo shim gives ordinary
# agent builds low MBX priority; promote this bounded presubmit unless the
# caller deliberately selected another priority.
export MBX_SCHEDULER_PRIORITY="${MBX_SCHEDULER_PRIORITY:-normal}"

# Several focused presubmits commonly overlap on this 10-core host. Letting
# every nextest process claim the CPU-count default turns four useful shards
# into forty competing test processes; observed load exceeded 200 and healthy
# tests stretched into timeout territory. Two-wide keeps aggregate agent work
# near host capacity while the serialized merge gate retains its normal width.
# A caller measuring an isolated shard can still choose an explicit width.
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-2}"

# `cargo` inside the agent env resolves to scripts/bin/cargo, which routes
# build subcommands through the mbx build cache when it is installed and not
# disabled (WITCHY_BUILD_CACHE=off). The wrapper clears below stay: they
# neutralise the global sccache config for the plain-cargo fallback, and the
# shim ignores them.
run_in_agent_env() {
    local env_args=(-u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= "PATH=$PWD/scripts/bin:$PATH")
    case "$AGENT_TARGET_DIR" in
        target | "$PWD/target") ;;
        *) env_args+=("CARGO_TARGET_DIR=$AGENT_TARGET_DIR") ;;
    esac
    if [ "$agent_target_from_gate" -eq 1 ] && [ -z "${CARGO_PROFILE_TEST_STRIP+x}" ]; then
        env_args+=(CARGO_PROFILE_TEST_STRIP=symbols)
    fi
    env "${env_args[@]}" "${agent_priority[@]}" "$@"
}

run_paths_checks() {
    "${agent_priority[@]}" ./scripts/test-for-paths.sh --run "$@"
}

explicit_paths_cover_tip() {
    [ "$#" -gt 0 ] || return 1
    local all_paths selected_paths
    all_paths="$(git diff --name-only master...HEAD -- 2>/dev/null | sort)"
    selected_paths="$(git diff --name-only master...HEAD -- "$@" 2>/dev/null | sort)"
    [ -n "$all_paths" ] && [ "$all_paths" = "$selected_paths" ]
}

run_edit_checks() {
    local explicit_paths="$#" started_at="$SECONDS"
    local edit_plan
    edit_plan="$(./scripts/test-for-paths.sh --edit-loop "$@")"
    if printf '%s\n' "$edit_plan" | grep -Eq '^  .*(cargo|check[.]sh)'; then
        ensure_agent_target
    fi
    run_in_agent_env ./scripts/test-for-paths.sh --edit-loop --run "$@"
    # With no explicit paths, test-for-paths derived its selection from the
    # entire branch diff. An explicit path list is equally safe only when its
    # pathspec expansion covers that complete committed diff; partial probes
    # must not certify the tip. Record only an exact clean commit even when the
    # cheapest route used named probes and never entered check.sh.
    if [ "$explicit_paths" -eq 0 ] || explicit_paths_cover_tip "$@"; then
        ./scripts/record-shard-receipt.sh edit "$((SECONDS - started_at))" || true
    fi
    # Advisory only: never let the notice change this command's outcome.
    report_bounded_edit_gap "$edit_plan" || true
}

# The edit loop is deliberately bounded so iteration stays fast (see the
# sentinel assertions in tools/merge-queue-tests). That makes it a weaker
# signal than the gate, and the difference is measurable: over the last 150
# queue attempts a branch carrying a green `edit` receipt went red 44.8% of
# the time, against 15.0% for branches with no receipt at all, and 13 of the
# 21 identified failures were in the `witchy::misc` binary -- which the
# bounded plan does not run in full. Say so once, here, where the agent is
# looking, rather than letting a green edit loop read as gate-ready.
report_bounded_edit_gap() {
    # This is an advisory about the selected plan, not another validation run.
    # Actual execution still asks the router to select its checks afresh.
    local plan="$1"
    case "$plan" in
        *"check.sh --fast"*) return 0 ;;
    esac
    # Only when a semantics file is in the diff: everything else is served by
    # its own focused tests.
    # `git` fails outside a repository (the agent-check QoS fixture runs this
    # script from a temporary directory), and a failing command substitution
    # is fatal under `set -e`. Swallow it: no diff means nothing to say.
    local changed
    changed="$( { git diff --name-only master...HEAD 2>/dev/null; git diff --name-only 2>/dev/null; } || true )"
    printf '%s\n' "$changed" | grep -qE '^crates/witchy-(types/src/(typeck|traits)|lower/src/(canonical_lower|analysis)|wir/src/canonical_safewir/check)' || return 0
    # ... and only when the bounded plan did not already run that binary whole.
    case "$plan" in
        *"--test misc"*)
            printf '%s\n' "$plan" | grep -qE -- "--test misc( |$)" && return 0 ;;
    esac
    printf '\n\033[1;33mbounded edit loop\033[0m: this change touches language semantics, and the gate also runs the whole `witchy::misc` binary (~271s), which this plan does not.\n'
    printf '  before submitting: ./scripts/nextest-admitted.sh witchy -- cargo nextest run -p witchy --features witchy-wir/native --test misc\n'
}

ensure_agent_target() {
    case "$AGENT_TARGET_DIR" in
        target | "$PWD/target") return 0 ;;
    esac
    if [ -d "$AGENT_TARGET_DIR" ]; then
        # Agents commonly seed this directory before invoking us. Keep the
        # gate's test profile for that existing seed too, or Cargo rebuilds
        # its dependency graph under a different profile.
        if [ -f "$AGENT_TARGET_DIR/.witchy-mbx-incremental-seed" ]; then
            agent_target_from_gate=1
        fi
        return 0
    fi
    if [ -d target ]; then
        ./scripts/worktree-warm.sh --target-dir "$AGENT_TARGET_DIR"
    else
        ./scripts/worktree-warm.sh --target-dir "$AGENT_TARGET_DIR" || \
            echo "agent-check: warm target unavailable; continuing with an action-cache target" >&2
        [ ! -f "$AGENT_TARGET_DIR/.witchy-mbx-incremental-seed" ] || agent_target_from_gate=1
    fi
}

default_agent_target() {
    local repo_root
    repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
        printf 'target-codex\n'
        return
    }
    if [ -n "$agent_main_worktree" ] && [ "$repo_root" != "$agent_main_worktree" ]; then
        # Always use the checkout-local mbx-managed target. A caller that
        # deliberately needs another directory can still set CARGO_TARGET_DIR.
        printf 'target\n'
    else
        # Agents sharing the main checkout still need independent Cargo locks.
        printf 'target-codex\n'
    fi
}

AGENT_TARGET_DIR="${CARGO_TARGET_DIR:-$(default_agent_target)}"
agent_target_from_gate=0
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
    edit)
        run_edit_checks "$@"
        ;;

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
        if [ "${AGENT_CHECK_RUNNER:-cargo}" = "nextest" ]; then
            cargo nextest --version >/dev/null 2>&1 || {
                echo "agent-check: AGENT_CHECK_RUNNER=nextest requires cargo-nextest" >&2
                exit 2
            }
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
