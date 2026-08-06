#!/usr/bin/env bash
# Canonical local operational-state paths. Source this file; it intentionally
# performs no I/O so report-only commands remain read-only.

witchy_state_root() { # witchy_state_root <main-worktree-root>
    local root="$1"
    printf '%s\n' "${WITCHY_STATE_DIR:-$root/state}"
}

witchy_merge_queue_state_dir() { # witchy_merge_queue_state_dir <main-worktree-root>
    local root="$1"
    if [ -n "${MERGE_QUEUE_STATE_DIR:-}" ]; then
        printf '%s\n' "$MERGE_QUEUE_STATE_DIR"
        return 0
    fi

    local state_root canonical legacy
    state_root="$(witchy_state_root "$root")"
    canonical="$state_root/merge-queue"
    legacy="$root/scratch/merge-queue"

    if [ -n "${WITCHY_STATE_DIR:-}" ]; then
        printf '%s\n' "$canonical"
        return 0
    fi

    if [ -L "$legacy" ] && { [ ! -e "$canonical" ] || [ ! "$legacy" -ef "$canonical" ]; }; then
        printf 'state-paths: legacy merge-queue symlink does not resolve to %s: %s\n' \
            "$canonical" "$legacy" >&2
        return 1
    fi
    if [ -e "$canonical" ] && [ -e "$legacy" ] && [ ! -L "$legacy" ]; then
        printf 'state-paths: split merge-queue state: both %s and %s are real paths\n' \
            "$canonical" "$legacy" >&2
        return 1
    fi

    # An explicit state root always wins. Otherwise preserve a pre-cutover
    # checkout until migrate-state moves it. Fresh checkouts and completed
    # cutovers use state/; the legacy symlink remains for older agents.
    if [ -e "$canonical" ] || [ -L "$legacy" ] || [ ! -e "$legacy" ]; then
        printf '%s\n' "$canonical"
    else
        printf '%s\n' "$legacy"
    fi
}

witchy_agent_state_dir() { # witchy_agent_state_dir <main-worktree-root>
    printf '%s/agents\n' "$(witchy_state_root "$1")"
}
