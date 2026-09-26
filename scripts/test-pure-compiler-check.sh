#!/usr/bin/env bash
# Exercise planning and fail-closed metadata reuse without compiling Witchy.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/witchy-pure-edit-test.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/scripts/bin"
cp "$root/scripts/pure-compiler-check.sh" "$root/scripts/nextest-admitted.sh" \
    "$root/scripts/rfc0165-deferred-filter.sh" "$root/scripts/rfc0165-deferred-tests.txt" \
    "$root/scripts/nightly-deferred-tests.txt" "$fixture/scripts/"
git -C "$fixture" init -q
git -C "$fixture" -c user.name=Fixture -c user.email=fixture@example.invalid \
    commit -qm initial --allow-empty
cat > "$fixture/scripts/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PURE_TEST_LOG"
case " $* " in
    *' --binaries-metadata '*) test "${WITCHY_SEED_REFRESH:-on}" = off ;;
esac
if [ "$1" = metadata ]; then
    jq -n '{packages: [
        {name:"witchy-canonical-source", id:"source", targets:[
            {name:"witchy_canonical_source",kind:["lib"],test:true},
            {name:"checked_frontend_minting",kind:["test"],test:true}]},
        {name:"witchy-types", id:"types", targets:[
            {name:"witchy_types",kind:["lib"],test:false},
            {name:"typeck",kind:["test"],test:true}]},
        {name:"witchy-wir", id:"wir", targets:[
            {name:"witchy_wir",kind:["lib"],test:false},
            {name:"unit",kind:["test"],test:true},
            {name:"future_contract",kind:["test"],test:true},
            {name:"rfc0165_native_gate",kind:["test"],test:true}]},
        {name:"witchy-lower", id:"lower", targets:[
            {name:"witchy_lower",kind:["lib"],test:false},
            {name:"compiler_contracts",kind:["test"],test:true}]}
    ]}'
elif [ "${2:-}" = list ]; then
    [ "${PURE_TEST_BUILD_FAIL:-0}" = 0 ] || exit 17
    jq -n --arg missing "${PURE_TEST_MISSING:-}" '{"rust-binaries": [
        ["source", "checked_frontend_minting"], ["types", "typeck"],
        ["wir", "unit"], ["wir", "future_contract"], ["lower", "compiler_contracts"]
    ] | map(select(.[1] != $missing) | {
        key: .[1], value: {"package-id":.[0], "binary-name":.[1],
            kind:"test", "binary-path":("/fixture/" + .[1])}
    }) | from_entries}'
fi
MOCK
chmod +x "$fixture/scripts/bin/cargo"
export PURE_TEST_LOG="$fixture/commands"
packages=(witchy-canonical-source witchy-types witchy-wir witchy-lower)
run() { bash "$fixture/scripts/pure-compiler-check.sh" "${packages[@]}"; }
run
test "$(grep -c '^nextest list ' "$PURE_TEST_LOG")" = 1
grep -F -- '--test future_contract' "$PURE_TEST_LOG" >/dev/null
grep -F -- 'nextest run -p witchy-canonical-source --lib --no-default-features' "$PURE_TEST_LOG" >/dev/null
test "$(grep -c -- '--lib' "$PURE_TEST_LOG")" = 1
if grep -E -- '--features|rfc0165_native_gate' "$PURE_TEST_LOG"; then exit 1; fi
grep -F -- 'package(=witchy-canonical-source) or package(=witchy-types) or package(=witchy-wir)' "$PURE_TEST_LOG" >/dev/null
grep -F -- '(package(=witchy-lower)) and (' "$PURE_TEST_LOG" >/dev/null
for failure in missing build; do
    : > "$PURE_TEST_LOG"
    if [ "$failure" = missing ]; then
        export PURE_TEST_MISSING=compiler_contracts
    else
        unset PURE_TEST_MISSING
        export PURE_TEST_BUILD_FAIL=1
    fi
    if run > "$fixture/$failure.log" 2>&1; then
        echo "pure compiler edit accepted $failure failure" >&2; exit 1
    fi
    # The source library may already have run, but no reused selection may.
    if grep -F -- '--binaries-metadata' "$PURE_TEST_LOG"; then exit 1; fi
done
unset PURE_TEST_BUILD_FAIL
if bash "$fixture/scripts/pure-compiler-check.sh" witchy-runtime >/dev/null 2>&1; then
    echo 'pure compiler edit accepted a native package' >&2; exit 1
fi
echo 'pure compiler edit contracts passed'
