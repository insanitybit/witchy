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
EOF
cat >"$root/scripts/merge-queue.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = submit ] || exit 22
shift
printf '%s\n' "$@" > merge-queue-args
EOF
chmod +x "$root/scripts/"*.sh

git -C "$root" init -q -b demo
git -C "$root" config user.email queue-test@witchy.invalid
git -C "$root" config user.name 'Witchy Queue Test'
touch "$root/base.txt"
git -C "$root" add base.txt
git -C "$root" commit -qm base

"$root/scripts/submit-with-presubmit.sh" demo "latency note"
grep -Fx 'agent-check edit' "$root/agent-check-called"
printf '%s\n' demo 'latency note' | cmp -s - "$root/merge-queue-args"

if "$root/scripts/submit-with-presubmit.sh" wrong >/dev/null 2>&1; then
    echo "branch mismatch was accepted" >&2
    exit 1
fi
echo "submit-with-presubmit: ok"
