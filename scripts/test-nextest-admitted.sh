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
printf '%s\n' "${WITCHY_CARGO_SHIM_ACTIVE:-}" > "$NEXTEST_ADMITTED_SHIM_LOG"
printf '%s\n' "$@" > "$NEXTEST_ADMITTED_LOG"
exit "${NEXTEST_ADMITTED_EXIT:-0}"
EOF
chmod +x "$tmp/bin/cargo"
export NEXTEST_ADMITTED_LOG="$tmp/args"
export NEXTEST_ADMITTED_SHIM_LOG="$tmp/shim-active"
# Keep this test hermetic: the repository shim must be exercised, then delegate
# to the fake cargo above instead of contacting the build-cache service.
export WITCHY_BUILD_CACHE=off

cd "$root"
selection='test(/^example_tests::/)'
policy="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt witchy)"
# The wrapper matches the landing gate, which excludes the nightly-deferred
# names too; build the same expectation here from the same list.
nightly_names=""
while IFS= read -r nightly_line || [ -n "$nightly_line" ]; do
    case "$nightly_line" in
        ''|'#'*) continue ;;
    esac
    nightly_names="${nightly_names:+$nightly_names|}^${nightly_line}\$"
done < scripts/nightly-deferred-tests.txt
policy="($policy) and not (test(/${nightly_names}/))"
PATH="$tmp/bin:$PATH" ./scripts/nextest-admitted.sh witchy -- \
    cargo nextest run --features witchy-wir/native -E "$selection"
[ "$(cat "$NEXTEST_ADMITTED_SHIM_LOG")" = 1 ] || fail 'repository cargo shim was bypassed'
[ "$(sed -n '1p' "$NEXTEST_ADMITTED_LOG")" = nextest ] || fail 'cargo subcommand changed'
[ "$(sed -n '2p' "$NEXTEST_ADMITTED_LOG")" = run ] || fail 'nextest action changed'
[ "$(sed -n '3p' "$NEXTEST_ADMITTED_LOG")" = --features ] || fail 'feature flag changed'
[ "$(sed -n '4p' "$NEXTEST_ADMITTED_LOG")" = witchy-wir/native ] || fail 'feature value changed'
[ "$(sed -n '5p' "$NEXTEST_ADMITTED_LOG")" = --bin ] || fail 'example matrix was not target-narrowed'
[ "$(sed -n '6p' "$NEXTEST_ADMITTED_LOG")" = witchy ] || fail 'example matrix selected the wrong binary'
[ "$(sed -n '7p' "$NEXTEST_ADMITTED_LOG")" = -E ] || fail 'filter flag missing'
[ "$(sed -n '8p' "$NEXTEST_ADMITTED_LOG")" = "($selection) and ($policy)" ] || fail 'filter composition changed'
[ "$(wc -l < "$NEXTEST_ADMITTED_LOG" | tr -d ' ')" -eq 8 ] || fail 'unexpected example-matrix arguments'

# An explicit target owns its compilation scope even when the filter text is
# the full example expression. Do not silently replace or expand it.
PATH="$tmp/bin:$PATH" ./scripts/nextest-admitted.sh witchy -- \
    cargo nextest run --test misc -E "$selection"
[ "$(sed -n '3p' "$NEXTEST_ADMITTED_LOG")" = --test ] || fail 'explicit target flag changed'
[ "$(sed -n '4p' "$NEXTEST_ADMITTED_LOG")" = misc ] || fail 'explicit target value changed'
if grep -Fxq -- --bin "$NEXTEST_ADMITTED_LOG"; then
    fail 'explicit target gained an implicit binary'
fi

# Command-module tests are also owned exclusively by src/main.rs. Both broad
# router expressions must avoid compiling every root integration target.
for command_selection in 'test(/^commands::/)' 'test(/^commands::web/)'; do
    PATH="$tmp/bin:$PATH" ./scripts/nextest-admitted.sh witchy -- \
        cargo nextest run -E "$command_selection"
    [ "$(sed -n '3p' "$NEXTEST_ADMITTED_LOG")" = --bin ] \
        || fail 'command selection was not target-narrowed'
    [ "$(sed -n '4p' "$NEXTEST_ADMITTED_LOG")" = witchy ] \
        || fail 'command selection selected the wrong binary'
    [ "$(sed -n '5p' "$NEXTEST_ADMITTED_LOG")" = -E ] \
        || fail 'command selection lost its filter flag'
    [ "$(sed -n '6p' "$NEXTEST_ADMITTED_LOG")" = "($command_selection) and ($policy)" ] \
        || fail 'command selection composition changed'
done

if NEXTEST_ADMITTED_EXIT=23 PATH="$tmp/bin:$PATH" \
    ./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run -p witchy-lower; then
    fail 'cargo failure was swallowed'
else
    result=$?
fi
[ "$result" -eq 23 ] || fail "cargo failure status changed to $result"
lower_policy="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt witchy-lower)"
lower_policy="($lower_policy) and not (test(/${nightly_names}/))"
[ "$(tail -2 "$NEXTEST_ADMITTED_LOG" | head -1)" = -E ] || fail 'unselected policy flag missing'
[ "$(tail -1 "$NEXTEST_ADMITTED_LOG")" = "$lower_policy" ] || fail 'package policy changed'
before="$(cat "$NEXTEST_ADMITTED_LOG")"
[ "$(PATH="$tmp/bin:$PATH" ./scripts/nextest-admitted.sh witchy-lower --filter)" = "$lower_policy" ] \
    || fail 'query policy differs from execution policy'
[ "$(cat "$NEXTEST_ADMITTED_LOG")" = "$before" ] || fail 'policy query invoked cargo'
# A nightly-deferred name must be excluded by the composed policy: a focused
# run that fails on a test the gate never runs teaches agents to ignore red.
sample="$(grep -vE '^\s*(#|$)' scripts/nightly-deferred-tests.txt | head -1)"
[ -n "$sample" ] || fail 'nightly-deferred list is empty'
case "$(tail -1 "$NEXTEST_ADMITTED_LOG")" in
    *"$sample"*) ;;
    *) fail "composed policy does not exclude the nightly-deferred '$sample'" ;;
esac

printf 'focused admitted selection and failure propagation passed\n'
