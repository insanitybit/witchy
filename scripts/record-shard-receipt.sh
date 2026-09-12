#!/usr/bin/env bash
# Record that the current clean commit passed a focused validation shard.
#
# Receipts are advisory and keyed by commit. A dirty tree is deliberately not
# recorded because the check covered HEAD plus uncommitted changes.
set -u

shard="${1:-}"
[ -n "$shard" ] || exit 0
[ -z "${WITCHY_GATE_SCOPE+x}" ] || exit 0

sha="$(git rev-parse HEAD 2>/dev/null)" || exit 0
[ -n "$sha" ] || exit 0
[ -z "$(git status --porcelain 2>/dev/null)" ] || exit 0

main_git="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || exit 0
main_root="$(dirname "$main_git")"
# shellcheck source=/dev/null
. "$main_root/scripts/state-paths.sh" 2>/dev/null || exit 0
dir="$(witchy_merge_queue_state_dir "$main_root" 2>/dev/null)/receipts" || exit 0
mkdir -p "$dir" 2>/dev/null || exit 0

prior='[]'
if [ -f "$dir/$sha.json" ]; then
    prior="$(jq -c '(.shards // [.shard]) | map(select(. != null))' \
        "$dir/$sha.json" 2>/dev/null || echo '[]')"
    [ -n "$prior" ] || prior='[]'
fi

tmp="$dir/$sha.json.tmp.$$"
if jq -cn --arg sha "$sha" --arg shard "$shard" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson prior "$prior" \
    '{sha:$sha, shards: ($prior + [$shard] | unique), at:$at}' \
    >"$tmp" 2>/dev/null; then
    mv "$tmp" "$dir/$sha.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
else
    rm -f "$tmp" 2>/dev/null
fi

find "$dir" -name '*.json' -type f -mtime +30 -delete 2>/dev/null || true
exit 0
