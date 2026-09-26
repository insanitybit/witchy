#!/usr/bin/env bash
# Run a focused nextest selection against the same admitted-test policy as the
# landing gate. Exact security probes intentionally call nextest directly.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# A caller batching packages can compose this same policy with unfiltered
# security suites, without duplicating the exclusion lists or starting Cargo.
print_filter=0
if [ "$#" -eq 2 ] && [ "$2" = --filter ]; then
    print_filter=1
    set -- "$1" -- cargo nextest run
fi
if [ "$#" -lt 4 ] || [ "$2" != -- ] || [ "$3" != cargo ] || [ "$4" != nextest ]; then
    echo "usage: $0 <package> {--filter | -- cargo nextest run [args...]}" >&2
    exit 2
fi

package="$1"
shift 2
command=("$@")
if [ "${command[2]:-}" != run ]; then
    echo "nextest-admitted: expected 'cargo nextest run'" >&2
    exit 2
fi

selection=""
filtered=()
explicit_scope=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        -E)
            [ -z "$selection" ] || {
                echo "nextest-admitted: multiple -E expressions" >&2
                exit 2
            }
            [ "$#" -ge 2 ] || {
                echo "nextest-admitted: -E requires an expression" >&2
                exit 2
            }
            selection="$2"
            shift 2
            ;;
        -p|--package|--workspace|--all|--lib|--bin|--bins|--test|--tests|--all-targets)
            explicit_scope=true
            filtered+=("$1")
            shift
            ;;
        -p?*|--package=*|--bin=*|--test=*)
            explicit_scope=true
            filtered+=("$1")
            shift
            ;;
        *) filtered+=("$1"); shift ;;
    esac
done

# These exact selections name modules declared only by src/main.rs, so Cargo
# needs the root binary and no integration-test targets. Without a target
# selector, Cargo builds every integration binary before nextest applies -E.
# The same narrowing at the route layer cut a measured warm run from 41s to
# 12s; one cold route spent 7m14s compiling 24 binaries to start one test.
# Keep the allowlist exact so compound selections and caller scopes cannot be
# silently narrowed.
root_bin_selection=false
case "$selection" in
    'test(/^example_tests::/)' | 'test(/^commands::/)' | 'test(/^commands::web/)')
        root_bin_selection=true
        ;;
esac
if [ "$package" = witchy ] \
    && [ "$root_bin_selection" = true ] \
    && [ "$explicit_scope" = false ]; then
    filtered+=(--bin witchy)
fi

policy="$(./scripts/rfc0165-deferred-filter.sh scripts/rfc0165-deferred-tests.txt "$package")"

# The landing gate excludes TWO lists, so a wrapper that applies only the
# RFC-0165 one does not match it. Measured 2026-09-20: a focused
# `--test misc` run through this wrapper reported 8 failures on a clean
# master, and all 8 were nightly-deferred names the gate never runs. A
# focused check that fails on tests the gate ignores trains agents to submit
# through red output, which is the opposite of what this wrapper is for.
# The list is the same single source of truth check.sh reads.
nightly_deferred_file="${WITCHY_GATE_DEFERRED_NIGHTLY:-scripts/nightly-deferred-tests.txt}"
if [ -r "$nightly_deferred_file" ]; then
    nightly_names=""
    while IFS= read -r nightly_line || [ -n "$nightly_line" ]; do
        case "$nightly_line" in
            ''|'#'*) continue ;;
        esac
        nightly_names="${nightly_names:+$nightly_names|}^${nightly_line}\$"
    done < "$nightly_deferred_file"
    if [ -n "$nightly_names" ]; then
        policy="($policy) and not (test(/${nightly_names}/))"
    fi
fi

if [ -n "$selection" ]; then
    policy="($selection) and ($policy)"
fi
if [ "$print_filter" -eq 1 ]; then
    printf '%s\n' "$policy"
    exit 0
fi
# Enter through the repository's cargo shim even when the caller has not
# prepended scripts/bin to PATH. The shim owns the MBX policy and still finds
# the caller's real cargo (or a hermetic test double) on PATH.
filtered[0]="$root/scripts/bin/cargo"
exec "${filtered[@]}" -E "$policy"
