#!/usr/bin/env bash
# Exercise focused-policy composition and exit propagation without a Rust build.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-nextest-admitted.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'nextest-admitted: %s\n' "$*" >&2; exit 1; }

mkdir "$tmp/bin"
cat > "$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$NEXTEST_ADMITTED_LOG"
exit "${NEXTEST_ADMITTED_EXIT:-0}"
EOF
chmod +x "$tmp/bin/cargo"
export NEXTEST_ADMITTED_LOG="$tmp/args"

cd "$root"
selection='test(/^example_tests::/)'
policy="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt witchy)"
PATH="$tmp/bin:$PATH" ./scripts/nextest-admitted.sh witchy -- \
    cargo nextest run --features witchy-wir/native -E "$selection"
[ "$(sed -n '1p' "$NEXTEST_ADMITTED_LOG")" = nextest ] || fail 'cargo subcommand changed'
[ "$(sed -n '2p' "$NEXTEST_ADMITTED_LOG")" = run ] || fail 'nextest action changed'
[ "$(sed -n '3p' "$NEXTEST_ADMITTED_LOG")" = --features ] || fail 'feature flag changed'
[ "$(sed -n '4p' "$NEXTEST_ADMITTED_LOG")" = witchy-wir/native ] || fail 'feature value changed'
[ "$(sed -n '5p' "$NEXTEST_ADMITTED_LOG")" = -E ] || fail 'filter flag missing'
[ "$(sed -n '6p' "$NEXTEST_ADMITTED_LOG")" = "($selection) and ($policy)" ] || fail 'filter composition changed'

if NEXTEST_ADMITTED_EXIT=23 PATH="$tmp/bin:$PATH" \
    ./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower; then
    fail 'cargo failure was swallowed'
else
    result=$?
fi
[ "$result" -eq 23 ] || fail "cargo failure status changed to $result"
lower_policy="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt witchy-lower)"
[ "$(tail -2 "$NEXTEST_ADMITTED_LOG" | head -1)" = -E ] || fail 'unselected policy flag missing'
[ "$(tail -1 "$NEXTEST_ADMITTED_LOG")" = "$lower_policy" ] || fail 'package policy changed'
printf 'focused admitted selection and failure propagation passed\n'
