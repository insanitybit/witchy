#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-agent-check-qos.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"

cat >"$tmp/bin/taskpolicy" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >"$AGENT_CHECK_QOS_LOG"
[ "$1" = -c ] && [ "$2" = utility ]
shift 2
exec "$@"
EOF
chmod +x "$tmp/bin/taskpolicy"

PATH="$tmp/bin:$PATH" AGENT_CHECK_QOS_LOG="$tmp/taskpolicy.log" \
    CARGO_TARGET_DIR="$tmp/target" \
    "$root/scripts/agent-check.sh" paths docs/agent-check-qos-probe.md >/dev/null

grep -F -- '-c utility ./scripts/test-for-paths.sh --run docs/agent-check-qos-probe.md' \
    "$tmp/taskpolicy.log" >/dev/null
echo "agent-check focused commands inherit utility QoS"
