#!/usr/bin/env bash
# Print the nextest filterset that excludes the RFC-0165 deferred-feature
# tests, from the one list both the merge gate (scripts/check.sh) and CI
# consume. The list lives in scripts/rfc0165-deferred-tests.txt: one
# `binary-id test-name` per line, `#` comments. Output is `all()` when the
# list is empty, otherwise `not ((binary_id(=B) and test(=T)) or ...)`, so
# the caller can always pass it as `-E`.
# An optional second argument selects one package's binaries. Validate the
# entire list before omitting other packages: scoping never hides a forbidden
# security-suite exclusion.
#
# Two invariants are enforced here rather than trusted: every entry names a
# tracked binary (nextest refuses a filterset whose binary_id matches
# nothing, which is what the first landing attempt hit), and no entry may
# reach the native security suites, which stay mandatory everywhere: the
# whole `witchy-wir` crate, the canonical `witchy-lower` tests, and the
# `rfc0165_*` and `canonical_bare_driver_*` tests, including nested modules.
# (The legacy `codegen` boundary tests in `witchy-lower`
# are a deferred family, not a security suite.)
set -euo pipefail
list="${1:-$(dirname "$0")/rfc0165-deferred-tests.txt}"
package="${2:-}"
if [ ! -r "$list" ]; then
    echo "rfc0165-deferred-filter: unreadable list '$list'" >&2
    exit 2
fi
terms=""
while read -r binary test _; do
    case "$binary" in ''|'#'*) continue ;; esac
    if [ -z "$test" ]; then
        echo "rfc0165-deferred-filter: entry without a test name: '$binary'" >&2
        exit 2
    fi
    case "$binary" in
        witchy-wir|witchy-wir::*)
            echo "rfc0165-deferred-filter: '$binary $test' would defer a native security suite" >&2
            exit 2 ;;
        witchy-lower|witchy-lower::*)
            case "$test" in
                *canonical*)
                    echo "rfc0165-deferred-filter: '$binary $test' would defer a canonical lowering test" >&2
                    exit 2 ;;
            esac ;;
    esac
    case "$test" in
        rfc0165_*|*::rfc0165_*|canonical_bare_driver_*|*::canonical_bare_driver_*)
            echo "rfc0165-deferred-filter: '$binary $test' would defer a foundation test" >&2
            exit 2 ;;
    esac
    if [ -n "$package" ]; then
        case "$binary" in
            "$package"|"$package"::*) ;;
            *) continue ;;
        esac
    fi
    terms="${terms:+$terms or }(binary_id(=$binary) and test(=$test))"
done < "$list"
if [ -n "$terms" ]; then
    printf 'not (%s)\n' "$terms"
else
    printf 'all()\n'
fi
