#!/usr/bin/env bash
set -euo pipefail

# The script is itself selected by agent-check, whose warm-target profile and
# scheduler priority and nextest width must not leak into the fixture cases.
unset CARGO_PROFILE_TEST_STRIP MBX_SCHEDULER_PRIORITY NEXTEST_TEST_THREADS

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

queue_state="$tmp/queue"
mkdir -p "$queue_state"

PATH="$tmp/bin:$PATH" AGENT_CHECK_QOS_LOG="$tmp/taskpolicy.log" \
    MERGE_QUEUE_STATE_DIR="$queue_state" \
    CARGO_TARGET_DIR="$tmp/target" \
    "$root/scripts/agent-check.sh" paths docs/agent-check-qos-probe.md >/dev/null

test ! -e "$tmp/taskpolicy.log"

mkdir "$queue_state/gate.lock"
PATH="$tmp/bin:$PATH" AGENT_CHECK_QOS_LOG="$tmp/taskpolicy.log" \
    MERGE_QUEUE_STATE_DIR="$queue_state" \
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
printf 'strip=%s priority=%s nextest=%s target=%s args=%s\n' \
    "${CARGO_PROFILE_TEST_STRIP-unset}" "${MBX_SCHEDULER_PRIORITY-unset}" \
    "${NEXTEST_TEST_THREADS-unset}" \
    "${CARGO_TARGET_DIR-unset}" "$*" \
    >>"$AGENT_CHECK_ENV_LOG"
EOF
chmod +x "$fixture/scripts/"*.sh

run_fixture() {
    shift
    (
        cd "$fixture"
        env -u CARGO_TARGET_DIR PATH="$tmp/bin:$PATH" \
            AGENT_CHECK_QOS_LOG="$tmp/taskpolicy.log" \
            AGENT_CHECK_WARM_LOG="$tmp/warm.log" \
            AGENT_CHECK_ENV_LOG="$tmp/env.log" \
            "$@" "$fixture/scripts/agent-check.sh" edit docs/probe.md
    )
}

run_fixture gate-seed env
grep -F 'strip=symbols priority=normal nextest=2 target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target-codex"
mkdir -p "$fixture/target"
run_fixture local-seed env
grep -F 'strip=unset priority=normal nextest=2 target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target" "$fixture/target-codex"
run_fixture explicit-symbols env CARGO_PROFILE_TEST_STRIP=none MBX_SCHEDULER_PRIORITY=low NEXTEST_TEST_THREADS=7
grep -F 'strip=none priority=low nextest=7 target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

rm -rf "$fixture/target-codex"
run_fixture shell-only env FAKE_AGENT_PLAN=shell
grep -F 'strip=unset priority=normal nextest=2 target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/env.log" >/dev/null

test "$(wc -l <"$tmp/warm.log" | tr -d ' ')" = 3

# A linked worktree reuses whichever private target the agent already seeded.
# Keep the non-git fixture above as the shared-checkout fallback, then exercise
# Git's actual linked-worktree metadata for both conventions here.
main_fixture="$tmp/main-repo"
linked_fixture="$tmp/linked-repo"
mkdir -p "$main_fixture/scripts"
cp "$root/scripts/agent-check.sh" "$main_fixture/scripts/agent-check.sh"
cp "$root/scripts/state-paths.sh" "$main_fixture/scripts/state-paths.sh"
cp "$fixture/scripts/test-for-paths.sh" "$main_fixture/scripts/test-for-paths.sh"
cp "$fixture/scripts/worktree-warm.sh" "$main_fixture/scripts/worktree-warm.sh"
git -C "$main_fixture" init -q
git -C "$main_fixture" config user.email test@example.com
git -C "$main_fixture" config user.name Test
git -C "$main_fixture" add scripts
git -C "$main_fixture" commit -qm initial
git -C "$main_fixture" worktree add -qb linked "$linked_fixture"
env -u CARGO_TARGET_DIR AGENT_CHECK_ENV_LOG="$tmp/linked-env.log" FAKE_AGENT_PLAN=shell \
    "$linked_fixture/scripts/agent-check.sh" edit docs/probe.md >/dev/null
grep -F 'strip=unset priority=normal nextest=2 target=target args=--edit-loop --run docs/probe.md' \
    "$tmp/linked-env.log" >/dev/null
mkdir -p "$linked_fixture/target-codex"
env -u CARGO_TARGET_DIR AGENT_CHECK_ENV_LOG="$tmp/linked-codex-env.log" \
    "$linked_fixture/scripts/agent-check.sh" edit docs/probe.md >/dev/null
grep -F 'strip=unset priority=normal nextest=2 target=target-codex args=--edit-loop --run docs/probe.md' \
    "$tmp/linked-codex-env.log" >/dev/null

echo "agent-check focused commands use utility QoS only while the gate is live"
echo "agent-check focused commands use normal MBX priority and preserve overrides"
echo "agent-check focused nextest uses bounded concurrency and preserves overrides"
echo "agent-check matches gate-seeded test profiles and preserves explicit symbols"
echo "agent-check skips target seeding for shell-only edit plans"
echo "agent-check reuses the seeded private target of a linked worktree"
