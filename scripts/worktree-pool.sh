# shellcheck shell=bash
# RFC-0171: a pool of recycled agent worktrees. Sourced by
# worktree-create.sh (claim/create), merge-queue.sh (release, from the sweep)
# and reclaim-build-caches.sh (age listing).
#
# rustc keys incremental state to the worktree's absolute path, so the only
# way a new task inherits it is to run at a path an earlier task built in.
# Pool worktrees live at <root>/.claude/worktrees/pool-<NN>. A FREE slot is
# detached at a landed master with a clean tree, and has a marker file
# <state>/worktree-pool/free/<NN> holding the epoch second it was freed.
# `mkdir <state>/worktree-pool/claims/<NN>` is the atomic claim that both the
# hook's hand-out and its creation of a new slot take, so two concurrent hooks
# never get the same slot.

worktree_pool_max() { printf '%s\n' "${WITCHY_WORKTREE_POOL_MAX:-6}"; }
worktree_pool_max_age_hours() { printf '%s\n' "${WITCHY_WORKTREE_POOL_MAX_AGE_HOURS:-24}"; }

worktree_pool_state_dir() {
    printf '%s/worktree-pool\n' "$(witchy_state_root "$1")"
}

worktree_pool_slot_path() {
    printf '%s/.claude/worktrees/pool-%s\n' "$1" "$2"
}

# Print the slot number (NN) of a pool path, or fail.
worktree_pool_slot_of() {
    local root="$1" wt="$2" slot
    # `git worktree list` prints resolved paths; the root may arrive through
    # a symlink (macOS /var -> /private/var). Compare resolved forms.
    root="$(cd -P "$root" 2>/dev/null && pwd)" || return 1
    wt="$(cd -P "$wt" 2>/dev/null && pwd)" || return 1
    case "$wt" in
        "$root"/.claude/worktrees/pool-[0-9][0-9]) ;;
        *) return 1 ;;
    esac
    slot="${wt##*/pool-}"
    printf '%s\n' "$slot"
}

# The sweep proved <wt> merged, clean and not re-queued. Detach it at master
# and mark it free instead of deleting it. Fails (and changes nothing the
# caller relies on) if <wt> is not a pool path or cannot be detached cleanly.
worktree_pool_release() {
    local root="$1" wt="$2" slot pool
    slot="$(worktree_pool_slot_of "$root" "$wt")" || return 1
    pool="$(worktree_pool_state_dir "$root")"
    mkdir -p "$pool/free" "$pool/claims" || return 1
    git -C "$wt" switch --quiet --detach master >/dev/null 2>&1 || return 1
    # Untracked files the queue and hook themselves leave behind.
    rm -f "$wt/.worktree-prebuild.log"
    rm -rf "$wt/target/cargo-timings"
    [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ] || return 1
    date +%s >"$pool/free/$slot"
}

# Remove a slot entirely: its worktree (with its target) and any marker.
worktree_pool_evict() {
    local root="$1" slot="$2" pool wt
    pool="$(worktree_pool_state_dir "$root")"
    wt="$(worktree_pool_slot_path "$root" "$slot")"
    rm -f "$pool/free/$slot"
    if [ -d "$wt" ]; then
        git -C "$root" worktree remove --force "$wt" >/dev/null 2>&1 || true
    fi
}

# Claim a free slot and check it out on a new <branch> at <base>. Prints the
# slot path on success. A slot that is stale (older than the max age), lost
# its build cache, or is not a clean detached checkout is evicted instead.
worktree_pool_claim() {
    local root="$1" branch="$2" base="$3" pool marker slot wt freed now max_age
    pool="$(worktree_pool_state_dir "$root")"
    [ -d "$pool/free" ] || return 1
    mkdir -p "$pool/claims" || return 1
    now="$(date +%s)"
    max_age=$(( $(worktree_pool_max_age_hours) * 3600 ))
    for marker in "$pool"/free/*; do
        [ -f "$marker" ] || continue
        slot="${marker##*/}"
        mkdir "$pool/claims/$slot" 2>/dev/null || continue
        wt="$(worktree_pool_slot_path "$root" "$slot")"
        freed="$(cat "$marker" 2>/dev/null || echo 0)"
        case "$freed" in '' | *[!0-9]*) freed=0 ;; esac
        if [ $((now - freed)) -gt "$max_age" ] \
            || [ ! -d "$wt/target/debug" ] \
            || [ "$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null)" != HEAD ] \
            || [ -n "$(git -C "$wt" status --porcelain 2>/dev/null)" ]; then
            echo "worktree-pool: evicting slot $slot (stale, cacheless or not clean)" >&2
            worktree_pool_evict "$root" "$slot"
            rmdir "$pool/claims/$slot" 2>/dev/null || true
            continue
        fi
        rm -f "$marker"
        if git -C "$wt" switch --quiet -c "$branch" "$base" 1>&2; then
            rmdir "$pool/claims/$slot" 2>/dev/null || true
            printf '%s\n' "$wt"
            return 0
        fi
        echo "worktree-pool: slot $slot refused the switch; evicting it" >&2
        worktree_pool_evict "$root" "$slot"
        rmdir "$pool/claims/$slot" 2>/dev/null || true
    done
    return 1
}

# Reserve the lowest unused slot number below the cap for a NEW pool worktree.
# Prints NN and leaves claims/NN held; the caller releases it with
# worktree_pool_unreserve once the worktree exists (or creation failed).
worktree_pool_reserve_new() {
    local root="$1" pool max i slot
    pool="$(worktree_pool_state_dir "$root")"
    mkdir -p "$pool/claims" || return 1
    max="$(worktree_pool_max)"
    for ((i = 0; i < max; i++)); do
        slot="$(printf '%02d' "$i")"
        [ -e "$(worktree_pool_slot_path "$root" "$slot")" ] && continue
        mkdir "$pool/claims/$slot" 2>/dev/null || continue
        # Re-check under the claim: another hook may have just finished it.
        if [ -e "$(worktree_pool_slot_path "$root" "$slot")" ]; then
            rmdir "$pool/claims/$slot" 2>/dev/null || true
            continue
        fi
        printf '%s\n' "$slot"
        return 0
    done
    return 1
}

worktree_pool_unreserve() {
    rmdir "$(worktree_pool_state_dir "$1")/claims/$2" 2>/dev/null || true
}
