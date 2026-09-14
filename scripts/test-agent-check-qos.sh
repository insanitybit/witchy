#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-agent-check-qos.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"

cat >"$tmp/bin/taskpolicy" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$AGENT_CHECK_QOS_LOG"
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

fixture="$tmp/fixture"
mkdir -p "$fixture/scripts"
cp "$root/scripts/agent-check.sh" "$fixture/scripts/agent-check.sh"
cat >"$fixture/scripts/worktree-warm.sh" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$AGENT_CHECK_WARM_LOG"
mkdir -p "$2"
EOF
cat >"$fixture/scripts/test-for-paths.sh" <<'EOF'
#!/bin/sh
set -eu
case " $* " in
    *' --run '*) ;;
    *)
        printf 'focused checks:\n'
        if [ "${FAKE_AGENT_PLAN:-cargo}" = cargo ]; then
            printf '  cargo test --test probe\n'
        else
            printf '  bash -n scripts/probe.sh\n'
        fi
        exit 0
        ;;
esac
printf 'strip=%s target=%s args=%s\n' \
    "${CARGO_PROFILE_TEST_STRIP-unset}" "${CARGO_TARGET_DIR-unset}" "$*" \
    >>"$AGENT_CHECK_ENV_LOG"
EOF
chmod +x "$fixture/scripts/"*.sh

run_fixture() {
    shift
    (
        cd "$fixture"
        env PATH="$tmp/bin:$PATH" \
            AGENT_CHECK_QOS_LOG="$tmp/taskpolicy.log" \
            AGENT_CHECK_WARM_LOG="$tmp/warm.log" \
            AGENT_CHECK_ENV_LOG="$tmp/env.log" \
            "$@" "$fixture/scripts/agent-check.sh" edit docs/probe.md
    )
}

run_fixture gate-seed env
grep -F 'strip=symbols target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target-codex"
mkdir -p "$fixture/target"
run_fixture local-seed env
grep -F 'strip=unset target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target" "$fixture/target-codex"
run_fixture explicit-symbols env CARGO_PROFILE_TEST_STRIP=none
grep -F 'strip=none target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target-codex"
run_fixture shell-only env FAKE_AGENT_PLAN=shell
grep -F 'strip=unset target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

test "$(wc -l <"$tmp/warm.log" | tr -d ' ')" = 3
echo "agent-check focused commands inherit utility QoS"
echo "agent-check matches gate-seeded test profiles and preserves explicit symbols"
echo "agent-check skips target seeding for shell-only edit plans"
