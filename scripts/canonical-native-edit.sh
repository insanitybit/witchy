#!/usr/bin/env bash
# The canonical edit route's fixed three scopes, not a shell-command parser.
# Called only by test-for-paths; the caller owns whole-diff receipt recording.
#
# Feature audit: the selected WIR predicates inspect canonical checker/graph/
# catalog state or parse source for authority pins. Raw encoder features only
# expose separate test APIs; these predicates do not branch on their absence.
# Lower's list_string test uses checked-source admission and a fixed Console
# fixture, with native-test-runtime explicitly enabled below. The recipe's
# codegen test also enables raw-module-test-api and asserts that the raw build
# route refuses. Root tests already use this native
# dev-feature union. No disabled-profile assertion is converted to an enabled
# one: optional runtime-native runs and every noncanonical route stay separate.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -ne 4 ] || [ -z "$1" ] || [ -z "$2" ] || [ -z "$3" ]; then
    echo "usage: canonical-native-edit.sh <wir-filter> <lower-filter> <root-filter> <build-recipe:0|1>" >&2
    exit 2
fi
wir_filter="$1" lower_filter="$2" root_filter="$3" recipe="$4"
case "$recipe" in 0|1) ;; *) exit 2 ;; esac
start_sha="$(git rev-parse HEAD)"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/witchy-canonical-edit.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
cargo_metadata="$temporary/cargo.json"
binaries_metadata="$temporary/binaries.json"
targets=(--bin witchy --test unit --test rfc0165_route_gate --test misc --test cli_subcommands --test codegen)
lower_scope='package(=witchy-lower) and kind(test) and binary(=codegen)'
features='witchy-wir/native,witchy-lower/native-test-runtime'

# One Cargo test build, with the feature union needed by the three scopes.
# These assertions live in integration targets, not cfg(test) library copies.
# --lib overrides lib.test=false and would rebuild three otherwise unused
# libraries. The lower execution probe requires its own runtime feature;
# enabling only WIR's native feature leaves that probe absent from inventory.
env -u WITCHY_ORACLE_EXAMPLE cargo metadata --locked --format-version 1 \
    --features "$features" > "$cargo_metadata"
env -u WITCHY_ORACLE_EXAMPLE cargo nextest list \
    -p witchy-wir -p witchy-lower -p witchy --features "$features" \
    "${targets[@]}" --list-type binaries-only --message-format json > "$binaries_metadata"

# A missing target must not become a partial green run. Nextest subsequently
# parses both complete documents and validates the reused binary paths itself.
[ -s "$cargo_metadata" ] && [ -s "$binaries_metadata" ]
jq -e '.packages | type == "array" and length > 0' "$cargo_metadata" >/dev/null
jq -e '."rust-binaries" | type == "object" and length > 0' "$binaries_metadata" >/dev/null
require_binary() {
    local package="$1" kind="$2" name="$3"
    jq -e --slurpfile cargo "$cargo_metadata" --arg package "$package" \
        --arg kind "$kind" --arg name "$name" '
        [$cargo[0].packages[] | select(.name == $package) | .id] as $ids |
        any(."rust-binaries"[];
            . as $binary | ($ids | index($binary."package-id")) != null
            and .kind == $kind and ($name == "" or ."binary-name" == $name)
            and (."binary-path" | type == "string" and length > 0))
    ' "$binaries_metadata" >/dev/null || {
        echo "canonical native: missing binary $package/$kind/$name" >&2
        exit 1
    }
}
require_binary witchy-wir test unit
require_binary witchy-wir test rfc0165_route_gate
require_binary witchy bin witchy
require_binary witchy test misc
require_binary witchy test cli_subcommands
require_binary witchy-lower test codegen

reuse=(--cargo-metadata "$cargo_metadata" --binaries-metadata "$binaries_metadata")
# Keep the two default-filter intersections, and root's explicit override.
# No shared -E union: identically named tests in another binary cannot leak in.
env -u WITCHY_ORACLE_EXAMPLE cargo nextest run "${reuse[@]}" \
    -E "package(=witchy-wir) and kind(test) and (binary(=unit) or binary(=rfc0165_route_gate)) and ($wir_filter)"
env -u WITCHY_ORACLE_EXAMPLE cargo nextest run "${reuse[@]}" \
    -E "$lower_scope and ($lower_filter)"
WITCHY_ORACLE_EXAMPLE=calc cargo nextest run "${reuse[@]}" --ignore-default-filter \
    -E "package(=witchy) and ((kind(bin) and binary(=witchy)) or (kind(test) and binary(=misc))) and ($root_filter)"
echo "canonical native: app group also probes fetch-root origin admission"
env -u WITCHY_ORACLE_EXAMPLE cargo nextest run "${reuse[@]}" \
    -E "package(=witchy) and kind(test) and binary(=cli_subcommands) and test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)"
if [ "$(git rev-parse HEAD)" != "$start_sha" ]; then
    echo "canonical native: HEAD changed during build/run; no receipt" >&2
    exit 1
fi
