#!/usr/bin/env bash
# Run a focused nextest selection against the same admitted-test policy as the
# landing gate. Exact security probes intentionally call nextest directly.
set -euo pipefail

if [ "$#" -lt 4 ] || [ "$2" != -- ] || [ "$3" != cargo ] || [ "$4" != nextest ]; then
    echo "usage: $0 <package> -- cargo nextest run [args...]" >&2
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
        *) filtered+=("$1"); shift ;;
    esac
done

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
exec "${filtered[@]}" -E "$policy"
