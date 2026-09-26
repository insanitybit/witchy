#!/usr/bin/env bash
# One build for the pure compiler integration tests selected by test-for-paths.
# Discover targets from Cargo, not a second list of each crate's test binaries.
# WIR's explicit native-only guard is the same exception as the pure gate.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$PWD/scripts/bin:$PATH"

[ "$#" -gt 0 ] || { echo 'pure compiler edit: no packages' >&2; exit 2; }
packages=("$@")
for package in "${packages[@]}"; do
    case "$package" in
        witchy-canonical-source | witchy-types | witchy-wir | witchy-lower) ;;
        *) echo "pure compiler edit: unsupported package $package" >&2; exit 2 ;;
    esac
done
start_sha="$(git rev-parse HEAD)"
temporary="$(mktemp -d "${TMPDIR:-/tmp}/witchy-pure-edit.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT
metadata="$temporary/cargo.json"
binaries="$temporary/binaries.json"
cargo metadata --locked --format-version 1 > "$metadata"

args=()
targets=()
direct=""
lower=0
for package in "${packages[@]}"; do
    jq -e --arg p "$package" 'any(.packages[]; .name == $p)' "$metadata" >/dev/null
    args+=(-p "$package")
    if [ "$package" = witchy-lower ]; then
        lower=1
    else
        direct="${direct:+$direct or }package(=$package)"
    fi
    # --lib on the combined command would force cfg(test) copies of the large
    # compiler crates whose manifests deliberately set lib.test=false. Run
    # only libraries that actually own tests, without forcing the others.
    if jq -e --arg p "$package" '
        any(.packages[] | select(.name == $p) | .targets[];
            .test and (.kind | index("lib") != null))' "$metadata" >/dev/null; then
        cargo nextest run -p "$package" --lib --no-default-features
    fi
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        targets+=(--test "$target")
    done < <(jq -r --arg p "$package" '
        .packages[] | select(.name == $p) | .targets[] |
        select(.test and (.kind | index("test") != null)) |
        select($p != "witchy-wir" or .name != "rfc0165_native_gate") | .name
    ' "$metadata")
done
[ "${#targets[@]}" -gt 0 ] || { echo 'pure compiler edit: no test targets' >&2; exit 1; }

cargo nextest list "${args[@]}" "${targets[@]}" --no-default-features \
    --list-type binaries-only --message-format json > "$binaries"
# Refuse missing targets rather than reusing an incomplete binary inventory.
for package in "${packages[@]}"; do
    jq -e --slurpfile cargo "$metadata" --arg p "$package" '
        [$cargo[0].packages[] | select(.name == $p) | .id] as $ids |
        [$cargo[0].packages[] | select(.name == $p) | .targets[] |
            select(.test and (.kind | index("test") != null)) |
            select($p != "witchy-wir" or .name != "rfc0165_native_gate") | .name] as $names |
        . as $inventory | all($names[]; . as $name |
            any($inventory."rust-binaries"[];
                . as $b | ($ids | index($b."package-id")) != null and
                .kind == "test" and ."binary-name" == $name and
                (."binary-path" | type == "string" and length > 0)))
    ' "$binaries" >/dev/null || { echo "pure compiler edit: missing $package target" >&2; exit 1; }
done
reuse=(--cargo-metadata "$metadata" --binaries-metadata "$binaries")
# These calls do not invoke rustc. Keep MBX's test scheduling, but do not
# refresh a native build seed merely to execute this pure binary inventory.
if [ -n "$direct" ]; then
    WITCHY_SEED_REFRESH=off cargo nextest run "${reuse[@]}" -E "$direct"
fi
if [ "$lower" -eq 1 ]; then
    WITCHY_SEED_REFRESH=off ./scripts/nextest-admitted.sh witchy-lower -- cargo nextest run "${reuse[@]}" \
        -E 'package(=witchy-lower)'
fi
[ "$(git rev-parse HEAD)" = "$start_sha" ] || {
    echo 'pure compiler edit: HEAD changed during checks' >&2; exit 1;
}
