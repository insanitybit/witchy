#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v tokei >/dev/null || {
    echo "test-footprint: tokei is required" >&2
    exit 1
}
command -v jq >/dev/null || {
    echo "test-footprint: jq is required" >&2
    exit 1
}

TMP="$(mktemp -d "${TMPDIR:-/tmp}/witchy-test-footprint.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

integration="$TMP/integration"
examples="$TMP/examples"
extracted="$TMP/extracted"
support="$TMP/support"
explicit="$TMP/explicit"
all="$TMP/all"

find tests -type f -name '*.rs' ! -path 'tests/support/*' | sort -u > "$integration"
find src/example_tests -type f -name '*.rs' | sort -u > "$examples"
find crates -type f -name '*.rs' \
    | rg '/src/([^/]*_tests\.rs|[^/]*_tests/.*\.rs)$' \
    | sort -u > "$extracted"
{
    find tests/support -type f -name '*.rs' 2>/dev/null || true
    find crates/witchy-testkit crates/witchy-test-host -type f -name '*.rs'
    test ! -f src/example_tests.rs || printf '%s\n' src/example_tests.rs
} | sort -u > "$support"

cat "$integration" "$examples" "$extracted" | sort -u > "$explicit"
cat "$explicit" "$support" | sort -u > "$all"

measure() {
    local label="$1"
    local paths="$2"
    local files
    files="$(wc -l < "$paths" | tr -d ' ')"
    if [[ "$files" == 0 ]]; then
        printf '%s\t0\t0\t0\t0\t0\n' "$label"
        return
    fi
    xargs tokei --type Rust --output json < "$paths" \
        | jq -r --arg label "$label" --arg files "$files" '
            (.Rust // {code: 0, comments: 0, blanks: 0}) as $rust
            | [
                $label,
                ($files | tonumber),
                ($rust.code + $rust.comments + $rust.blanks),
                $rust.code,
                $rust.comments,
                $rust.blanks
              ]
            | @tsv
        '
}

printf 'category\tfiles\trust_lines\tcode\tcomments\tblanks\n'
measure integration "$integration"
measure example_matrix "$examples"
measure extracted_crate_tests "$extracted"
measure explicit_total "$explicit"
measure support "$support"
measure explicit_plus_support "$all"

if [[ "${1:-}" == "--files" ]]; then
    printf '\nExplicit test files:\n'
    xargs tokei --type Rust --files < "$explicit"
    printf '\nTest support files:\n'
    xargs tokei --type Rust --files < "$support"
elif [[ $# -ne 0 ]]; then
    echo "usage: $0 [--files]" >&2
    exit 2
fi
