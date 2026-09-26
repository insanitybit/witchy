#!/usr/bin/env bash
# Share one native build between complete compiler suites and root examples.
# WIR/native only adds Wasmtime execution checks, not alternate admission logic.
# Lower's own native-test-runtime feature is NOT enabled. Pure-only routes still
# check non-native compilation; this mixed route does not prove portability.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$PWD/scripts/bin:$PATH"

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo 'usage: native-compiler-examples.sh <full|hash:1/8> [witchy-lower]' >&2
    exit 2
fi
partition=()
case "$1" in
    full) ;;
    hash:1/8) partition=(--partition "$1") ;;
    *) echo 'native compiler examples: unsupported partition' >&2; exit 2 ;;
esac
shift
include_lower=0
compiler_packages='["witchy-wir"]'
packages=(-p witchy-wir -p witchy)
if [ "$#" -eq 1 ]; then
    [ "$1" = witchy-lower ] || { echo 'native compiler examples: unsupported package' >&2; exit 2; }
    include_lower=1
    compiler_packages='["witchy-wir", "witchy-lower"]'
    packages+=(-p witchy-lower)
fi
start_sha="$(git rev-parse HEAD)"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/witchy-native-examples.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
metadata="$temporary/cargo.json"
binaries="$temporary/binaries.json"
expected="$temporary/targets.json"
cargo metadata --locked --format-version 1 --features witchy-wir/native > "$metadata"

# Cargo target flags apply to EVERY selected package. Discover every compiler
# integration inventory, rather than silently keeping only a fixed sentinel.
# Refuse unfamiliar target shapes so a future manifest change cannot lose tests.
jq -e --argjson names "$compiler_packages" '
    [.packages[] | select(.name as $name | $names | index($name))] as $compiler |
    ($compiler | length) == ($names | length) and
    all($compiler[].targets[]; (.test | not) or .kind == ["test"]) and
    all($compiler[]; any(.targets[]; .test and .kind == ["test"])) and
    any(.packages[] | select(.name == "witchy") | .targets[];
        .test and .kind == ["bin"] and .name == "witchy")
' "$metadata" >/dev/null || {
    echo 'native compiler examples: unsupported or missing package targets' >&2; exit 1;
}
jq --argjson names "$compiler_packages" '
    [.packages[] | select(.name as $name | $names | index($name)) | .id as $id |
    .targets[] | select(.test and .kind == ["test"]) |
    {package: $id, kind: "test", name: .name}] +
    [.packages[] | select(.name == "witchy") |
    {package: .id, kind: "bin", name: "witchy"}]' "$metadata" > "$expected"
jq -e 'length > 1' "$expected" >/dev/null
targets=(--bin witchy)
while IFS= read -r target; do
    targets+=(--test "$target")
done < <(jq -r '.[] | select(.kind == "test") | .name' "$expected")

cargo nextest list "${packages[@]}" --features witchy-wir/native \
    "${targets[@]}" --list-type binaries-only --message-format json > "$binaries"
jq -e --slurpfile expected "$expected" '
    . as $inventory | all($expected[0][]; . as $target |
        any($inventory."rust-binaries"[];
            ."package-id" == $target.package and .kind == $target.kind and
            ."binary-name" == $target.name and
            (."binary-path" | type == "string" and length > 0)))
' "$binaries" >/dev/null || {
    echo 'native compiler examples: incomplete binary inventory' >&2; exit 1;
}

reuse=(--cargo-metadata "$metadata" --binaries-metadata "$binaries")
# WIR retains its complete direct suite, including native guards. Lower keeps
# its own admitted policy. Only root examples use the example partition.
selection='package(=witchy-wir)'
if [ "$include_lower" -eq 1 ]; then
    lower_policy="$(./scripts/nextest-admitted.sh witchy-lower --filter)"
    selection="($selection) or (package(=witchy-lower) and ($lower_policy))"
fi
cargo nextest run "${reuse[@]}" -E "$selection"
policy="$(./scripts/nextest-admitted.sh witchy --filter)"
cargo nextest run "${reuse[@]}" ${partition[0]+"${partition[@]}"} \
    -E "package(=witchy) and kind(bin) and binary(=witchy) and test(/^example_tests::/) and ($policy)"
[ "$(git rev-parse HEAD)" = "$start_sha" ] || {
    echo 'native compiler examples: HEAD changed during checks' >&2; exit 1;
}
