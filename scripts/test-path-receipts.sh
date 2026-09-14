#!/usr/bin/env bash
set -euo pipefail

root="$(mktemp -d "${TMPDIR:-/tmp}/witchy-path-receipts.XXXXXX")"
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/repo/scripts" "$root/bin" "$root/state"
cp scripts/test-for-paths.sh scripts/record-shard-receipt.sh scripts/state-paths.sh \
    "$root/repo/scripts/"
cat >"$root/bin/python3" <<'SH'
#!/bin/sh
[ "${FAKE_PYTHON_FAIL:-0}" = 0 ]
SH
chmod +x "$root/bin/python3"

cd "$root/repo"
git init -q
git symbolic-ref HEAD refs/heads/master
git config user.name "Witchy Test"
git config user.email witchy-test@example.invalid
: >scripts/test-gate-test-cost.py
git add scripts
git commit -qm base
git switch -qc topic
printf '# changed\n' >scripts/test-gate-test-cost.py
git add scripts/test-gate-test-cost.py
git commit -qm change
sha="$(git rev-parse HEAD)"
receipt="$root/state/receipts/$sha.json"

env -u WITCHY_GATE_SCOPE PATH="$root/bin:$PATH" MERGE_QUEUE_STATE_DIR="$root/state" \
    bash scripts/test-for-paths.sh --run >/dev/null
jq -e --arg sha "$sha" \
    '.sha == $sha and (.shards | index("paths") != null) and (.durations_s.paths | type == "number")' \
    "$receipt" >/dev/null

rm -f "$receipt"
env -u WITCHY_GATE_SCOPE PATH="$root/bin:$PATH" MERGE_QUEUE_STATE_DIR="$root/state" \
    bash scripts/test-for-paths.sh --run scripts/test-gate-test-cost.py >/dev/null
[ ! -e "$receipt" ] || { echo "explicit path wrote a whole-diff receipt" >&2; exit 1; }

if env -u WITCHY_GATE_SCOPE PATH="$root/bin:$PATH" FAKE_PYTHON_FAIL=1 \
    MERGE_QUEUE_STATE_DIR="$root/state" bash scripts/test-for-paths.sh --run \
    >/dev/null 2>&1; then
    echo "failing focused command unexpectedly succeeded" >&2
    exit 1
fi
[ ! -e "$receipt" ] || { echo "failed command wrote a green receipt" >&2; exit 1; }

echo "test-path-receipts: ok"
