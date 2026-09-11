#!/usr/bin/env bash
# Exercise policy selection and failure propagation without a Cargo build.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-native-policy.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fail() { printf 'native-gate-policy: %s\n' "$*" >&2; exit 1; }
cat > "$tmp/list" <<'EOF'
witchy-lower codegen::legacy_fixture
witchy::misc ordinary::legacy_fixture
witchy-lower::legacy_integration ordinary::legacy_integration_fixture
EOF
lower='not ((binary_id(=witchy-lower) and test(=codegen::legacy_fixture)) or (binary_id(=witchy-lower::legacy_integration) and test(=ordinary::legacy_integration_fixture)))'
full='not ((binary_id(=witchy-lower) and test(=codegen::legacy_fixture)) or (binary_id(=witchy::misc) and test(=ordinary::legacy_fixture)) or (binary_id(=witchy-lower::legacy_integration) and test(=ordinary::legacy_integration_fixture)))'
[ "$(bash "$root/scripts/rfc0165-deferred-filter.sh" "$tmp/list" witchy-lower)" = "$lower" ] || fail 'package selection drift'
[ "$(bash "$root/scripts/rfc0165-deferred-filter.sh" "$tmp/list")" = "$full" ] || fail 'workspace selection drift'
[ "$(bash "$root/scripts/rfc0165-deferred-filter.sh" "$tmp/list" absent-package)" = 'all()' ] || fail 'empty selection drift'

# Even out-of-scope forbidden entries must reject. A scoped native job must
# never silently accept a list that another job would reject as unsafe.
for entry in \
    'witchy-wir check::sentinel' \
    'witchy-lower canonical_lower::sentinel' \
    'witchy::misc rfc0165_native_sentinel'; do
    printf '%s\n' "$entry" > "$tmp/forbidden"
    if bash "$root/scripts/rfc0165-deferred-filter.sh" "$tmp/forbidden" unrelated-package > "$tmp/out" 2> "$tmp/err"; then
        fail "forbidden exclusion accepted: $entry"
    else
        result=$?
        [ "$result" -eq 2 ] || fail "unexpected rejection status: $result"
    fi
    case "$(cat "$tmp/err")" in
        *'would defer'*) ;;
        *) fail 'rejection did not come from the security policy' ;;
    esac
done

mkdir "$tmp/bin"
cat > "$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$NATIVE_POLICY_LOG"
case "$*" in
    'test -p witchy-wir --features native --tests')
        [ "${NATIVE_POLICY_FAIL:-}" != wir ] || exit 31 ;;
    "nextest run -p witchy-lower --features witchy-wir/native --tests -E $NATIVE_POLICY_FILTER")
        [ "${NATIVE_POLICY_FAIL:-}" != lower ] || exit 32 ;;
    *) printf 'unexpected Cargo command: %s\n' "$*" >&2; exit 90 ;;
esac
EOF
chmod +x "$tmp/bin/cargo"
export NATIVE_POLICY_LOG="$tmp/cargo.log"
export NATIVE_POLICY_FILTER
NATIVE_POLICY_FILTER="$(bash "$root/scripts/rfc0165-deferred-filter.sh" "$root/scripts/rfc0165-deferred-tests.txt" witchy-lower)"
: > "$NATIVE_POLICY_LOG"
PATH="$tmp/bin:$PATH" bash "$root/scripts/rfc0165-native-gate.sh"
[ "$(wc -l < "$NATIVE_POLICY_LOG")" -eq 2 ] || fail 'native suites were not both invoked'
for mode in wir lower; do
    : > "$NATIVE_POLICY_LOG"
    if NATIVE_POLICY_FAIL="$mode" PATH="$tmp/bin:$PATH" bash "$root/scripts/rfc0165-native-gate.sh"; then
        fail "$mode failure was swallowed"
    else
        result=$?
    fi
    if [ "$mode" = wir ]; then
        [ "$result" -eq 31 ] || fail 'WIR failure status changed'
        [ "$(wc -l < "$NATIVE_POLICY_LOG")" -eq 1 ] || fail 'continued after WIR failure'
    else
        [ "$result" -eq 32 ] || fail 'lowerer failure status changed'
        [ "$(wc -l < "$NATIVE_POLICY_LOG")" -eq 2 ] || fail 'lowerer suite was not invoked'
    fi
done
printf 'native-gate policy selection and failure propagation passed\n'
