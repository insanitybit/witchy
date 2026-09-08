#!/usr/bin/env bash
# usage: queue-watch.sh <change-id>; writes the terminal journal event to status/queue-watch.out.
source "$(dirname "$0")/common.sh"
main=$(git worktree list | head -1 | awk '{print $1}')
journal="$main/state/merge-queue/journal.jsonl"
id=$1
rm -f "$STATUS/queue-watch.out"
nohup bash -c "for i in \$(seq 1 360); do line=\$(grep '\"$id\"' '$journal' 2>/dev/null | grep -E '\"event\": *\"(merged|red|dropped|gate_failed|failed)\"' | tail -1); if [ -n \"\$line\" ]; then echo \"\$line\" > '$STATUS/queue-watch.out'; exit 0; fi; sleep 10; done; echo timeout > '$STATUS/queue-watch.out'" >/dev/null 2>&1 &
disown
echo "watching $id; poll $STATUS/queue-watch.out"
