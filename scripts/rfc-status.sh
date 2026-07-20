#!/usr/bin/env bash
# Report whether non-terminal RFCs have durable live work.
#
#   ./scripts/rfc-status.sh          # concise report
#   ./scripts/rfc-status.sh --all    # include terminal RFCs
#   ./scripts/rfc-status.sh --check  # fail on stale/invalid RFC state
set -euo pipefail

check=0
show_all=0
for arg in "$@"; do
    case "$arg" in
        --check) check=1 ;;
        --all) show_all=1 ;;
        -h|--help) sed -n '2,/^set -euo pipefail$/{ /^set -euo pipefail$/d; s/^# \{0,1\}//; p; }' "$0"; exit 0 ;;
        *) echo "rfc-status: unknown arg '$arg'" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" worktree list --porcelain | sed -n '1s/^worktree //p')"
. "$here/scripts/state-paths.sh"
queue_dir="$(witchy_merge_queue_state_dir "$root")/queue"

queued_branches=""
if [ -d "$queue_dir" ]; then
    queued_branches="$(cat "$queue_dir"/*.json 2>/dev/null | jq -r '.branch // empty' || true)"
fi
is_queued() { [ -n "$queued_branches" ] && grep -Fqx -- "$1" <<<"$queued_branches"; }

branch_rfc_id() {
    local branch="$1"
    if [[ "$branch" =~ rfc[-_/]?([0-9][0-9][0-9][0-9]) ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
    fi
}

# Inventory worktrees and relevant branches once. Re-running `git worktree
# list` for every RFC/branch pair made the report itself a coordination tax in
# repositories with many abandoned worktrees.
rfc_records="$(awk '
    function emit() {
        if (id ~ /^[0-9][0-9][0-9][0-9]$/) {
            gsub(/^"|"$/, "", status)
            gsub(/^"|"$/, "", tracking)
            print file "\t" id "\t" status "\t" tracking
        }
    }
    FNR == 1 {
        if (seen) emit()
        seen = 1
        file = FILENAME
        id = status = tracking = ""
    }
    /^rfc:[[:space:]]*/ { line = $0; sub(/^rfc:[[:space:]]*/, "", line); id = line }
    /^status:[[:space:]]*/ { line = $0; sub(/^status:[[:space:]]*/, "", line); status = line }
    /^tracking:[[:space:]]*/ { line = $0; sub(/^tracking:[[:space:]]*/, "", line); tracking = line }
    END { if (seen) emit() }
' "$root"/rfcs/[0-9][0-9][0-9][0-9]-*.md)"

active_ids=""
while IFS=$'\t' read -r _file id status _tracking; do
    case "$status" in
        implemented|deferred|rejected|superseded) ;;
        *) active_ids="${active_ids}${id}"$'\n' ;;
    esac
done <<<"$rfc_records"

worktree_inventory="$(git -C "$root" worktree list --porcelain | awk '
    $1 == "worktree" { worktree = substr($0, 10) }
    $1 == "branch" { sub("refs/heads/", "", $2); print $2 "\t" worktree }
')"

branch_states=""
pickup_candidates=""
if git -C "$root" for-each-ref --count=1 --format='%(ahead-behind:master)' refs/heads >/dev/null 2>&1; then
    branches="$(git -C "$root" for-each-ref --format='%(refname:short)%09%(objectname:short)%09%(ahead-behind:master)' refs/heads)"
else
    branches="$(git -C "$root" for-each-ref --format='%(refname:short)%09%(objectname:short)' refs/heads | while IFS=$'\t' read -r branch sha; do
        ahead="$(git -C "$root" rev-list --count "master..$branch")"
        printf '%s\t%s\t%s 0\n' "$branch" "$sha" "$ahead"
    done)"
fi
while IFS=$'\t' read -r branch sha counts; do
    [ -n "$branch" ] || continue
    id="$(branch_rfc_id "$branch")"
    [ -n "$id" ] || continue
    grep -Fqx -- "$id" <<<"$active_ids" || continue
    if is_queued "$branch"; then
        branch_states="${branch_states}${id}"$'\t'"${branch}"$'\tQUEUED\n'
        continue
    fi
    worktree="$(awk -F $'\t' -v branch="$branch" '$1 == branch { print $2; exit }' <<<"$worktree_inventory")"
    if [ -n "$worktree" ] && [ -n "$(git -C "$worktree" status --porcelain 2>/dev/null || true)" ]; then
        branch_states="${branch_states}${id}"$'\t'"${branch}@${worktree}"$'\tDIRTY\n'
        continue
    fi
    ahead="${counts%% *}"
    if [ "$ahead" != "0" ]; then
        pickup_candidates="${pickup_candidates}${id}"$'\t'"${branch}"$'\t'"${sha}"$'\n'
    fi
done <<<"$branches"

# Patch-equivalent branches are ordinary queue/rebase residue, not dropped
# work. Check equivalence only for RFCs that have no queued or dirty branch;
# active RFC-0080-style branch families therefore do not multiply this cost.
while IFS=$'\t' read -r id branch sha; do
    [ -n "$id" ] || continue
    active="$(awk -F $'\t' -v id="$id" '$1 == id && ($3 == "QUEUED" || $3 == "DIRTY") { print $3; exit }' <<<"$branch_states")"
    [ -z "$active" ] || continue
    existing="$(awk -F $'\t' -v id="$id" '$1 == id && $3 == "PICKUP" { print $3; exit }' <<<"$branch_states")"
    [ -z "$existing" ] || continue
    cherry="$(git -C "$root" cherry master "$branch" 2>/dev/null || true)"
    if grep -q '^+' <<<"$cherry"; then
        branch_states="${branch_states}${id}"$'\t'"${branch}@${sha}"$'\tPICKUP\n'
    fi
done <<<"$pickup_candidates"

problems=0
reported=0
terminal=0
printf '%s\n' "RFC durable-state report:"
while IFS=$'\t' read -r _file id status tracking; do
    name="RFC-$id"

    case "$status" in
        implemented|deferred|rejected|superseded)
            terminal=$((terminal + 1))
            if [ "$show_all" -eq 1 ]; then
                printf '  %-9s %-12s TERMINAL\n' "$name" "$status"
                reported=$((reported + 1))
            fi
            continue
            ;;
    esac

    queued="$(awk -F $'\t' -v id="$id" '$1 == id && $3 == "QUEUED" { print $2; exit }' <<<"$branch_states")"
    dirty="$(awk -F $'\t' -v id="$id" '$1 == id && $3 == "DIRTY" { print $2; exit }' <<<"$branch_states")"
    pickup="$(awk -F $'\t' -v id="$id" '$1 == id && $3 == "PICKUP" { print $2; exit }' <<<"$branch_states")"

    state=""
    detail=""
    bad=0
    if [ -n "$queued" ]; then
        state="QUEUED"
        detail="$queued"
    elif [ -n "$dirty" ]; then
        state="DIRTY"
        detail="$dirty; commit a coherent pickup slice or record an exact blocker"
        bad=1
    elif [ -n "$pickup" ]; then
        state="PICKUP"
        detail="$pickup; validate and queue, or explicitly disposition it"
        bad=1
    else
        case "$status" in
            accepted)
                if [ -n "$tracking" ]; then
                    state="TRACKED"
                    detail="accepted policy/plan with tracking"
                else
                    state="STALE"
                    detail="accepted with no queue, branch, or tracking"
                    bad=1
                fi
                ;;
            proposed|planned)
                state="STALE"
                detail="$status with no queued or recoverable implementation"
                bad=1
                ;;
            in-progress)
                state="INVALID"
                detail="vague status is not part of the RFC lifecycle"
                bad=1
                ;;
            *)
                state="INVALID"
                detail="unknown status '$status'"
                bad=1
                ;;
        esac
    fi

    printf '  %-9s %-12s %-8s %s\n' "$name" "$status" "$state" "$detail"
    reported=$((reported + 1))
    if [ "$bad" -eq 1 ]; then
        problems=$((problems + 1))
    fi
done <<<"$rfc_records"

if [ "$reported" -eq 0 ]; then
    printf '%s\n' "  no non-terminal RFCs"
fi
printf 'summary: terminal=%d reported=%d problems=%d\n' "$terminal" "$reported" "$problems"

if [ "$check" -eq 1 ] && [ "$problems" -ne 0 ]; then
    exit 1
fi
