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
if [ -n "$selection" ]; then
    policy="($selection) and ($policy)"
fi
exec "${filtered[@]}" -E "$policy"
