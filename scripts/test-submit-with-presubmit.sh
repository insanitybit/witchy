#!/usr/bin/env bash
set -euo pipefail

root="$(mktemp -d "${TMPDIR:-/tmp}/witchy-submit-with-presubmit.XXXXXX")"
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/scripts"
cp scripts/submit-with-presubmit.sh "$root/scripts/submit-with-presubmit.sh"

cat >"$root/scripts/agent-check.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = edit ] || exit 21
printf 'agent-check %s\n' "$*" > agent-check-called
if [ -n "${FAKE_AGENT_LOG:-}" ]; then
    printf 'agent-check %s\n' "$*" > "$FAKE_AGENT_LOG"
fi
EOF
cat >"$root/scripts/merge-queue.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = submit ] || exit 22
shift
printf '%s\n' "$@" > merge-queue-args
EOF
chmod +x "$root/scripts/"*.sh

git -C "$root" init -q -b master
git -C "$root" config user.email queue-test@witchy.invalid
git -C "$root" config user.name 'Witchy Queue Test'
touch "$root/base.txt"
printf 'agent-check-called\nmerge-queue-args\n' > "$root/.gitignore"
git -C "$root" add base.txt
git -C "$root" add .gitignore
git -C "$root" add scripts
git -C "$root" commit -qm base
git -C "$root" switch -qc demo

"$root/scripts/submit-with-presubmit.sh" demo "latency note"
grep -Fx 'agent-check edit' "$root/agent-check-called"
printf '%s\n' demo 'latency note' | cmp -s - "$root/merge-queue-args"

rm -f "$root/agent-check-called" "$root/merge-queue-args"
"$root/scripts/submit-with-presubmit.sh"
grep -Fx 'agent-check edit' "$root/agent-check-called"
printf '%s\n' demo | cmp -s - "$root/merge-queue-args"

if "$root/scripts/submit-with-presubmit.sh" wrong >/dev/null 2>&1; then
    echo "branch mismatch was accepted" >&2
    exit 1
fi

git -C "$root" switch master -q
git -C "$root" switch -qc topic
git -C "$root" switch master -q
FAKE_AGENT_LOG="$root/detached-agent-called" \
    "$root/scripts/submit-with-presubmit.sh" --detached topic "detached note"
grep -Fx 'agent-check edit' "$root/detached-agent-called"
printf '%s\n' topic 'detached note' | cmp -s - "$root/merge-queue-args"

touch "$root/dirty-master-file"
if "$root/scripts/submit-with-presubmit.sh" --detached topic >/dev/null 2>&1; then
    echo "dirty shared master was accepted" >&2
    exit 1
fi
rm -f "$root/dirty-master-file"
echo "submit-with-presubmit: ok"
