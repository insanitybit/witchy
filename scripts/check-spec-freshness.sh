#!/usr/bin/env bash
# Validate optional `verified: <commit>` stamps in spec/*.md and report their
# distance from HEAD. Commit age is an advisory proxy, not proof that prose is
# stale; --strict makes the configured age threshold fail for release audits.
set -euo pipefail
cd "$(dirname "$0")/.."

strict=0
max_commits="${SPEC_FRESHNESS_MAX_COMMITS:-250}"

usage() {
    cat <<'EOF'
Usage: ./scripts/check-spec-freshness.sh [--strict] [--max-commits N]

Validate every optional `verified: <commit>` stamp in spec/*.md. Invalid or
non-ancestor stamps always fail. Stamps more than N commits behind HEAD are
advisory by default and fail with --strict (default N: 250).
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --strict)
            strict=1
            shift
            ;;
        --max-commits)
            [ "$#" -ge 2 ] || { echo "check-spec-freshness: --max-commits needs a value" >&2; exit 2; }
            max_commits="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "check-spec-freshness: unknown argument '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$max_commits" in
    '' | *[!0-9]*)
        echo "check-spec-freshness: max commit age must be a non-negative integer" >&2
        exit 2
        ;;
esac

stamped=0
unstamped=0
stale=0
errors=0

for file in spec/*.md; do
    stamps=$(awk '
        NR == 1 && $0 == "---" { frontmatter = 1; next }
        frontmatter && $0 == "---" { exit }
        frontmatter && /^verified:/ { print }
    ' "$file")
    count=$(printf '%s\n' "$stamps" | grep -c '^verified:' || true)
    if [ "$count" -eq 0 ]; then
        unstamped=$((unstamped + 1))
        continue
    fi
    if [ "$count" -ne 1 ]; then
        echo "ERROR $file: expected at most one verified stamp, found $count" >&2
        errors=$((errors + 1))
        continue
    fi

    stamp=$(
        printf '%s\n' "$stamps" |
            sed -n 's/^verified:[[:space:]]*\([0-9a-fA-F][0-9a-fA-F]*\)[[:space:]]*$/\1/p'
    )
    if [ -z "$stamp" ] || [ "${#stamp}" -lt 7 ] || [ "${#stamp}" -gt 40 ]; then
        echo "ERROR $file: verified stamp must be a 7-40 digit hexadecimal commit" >&2
        errors=$((errors + 1))
        continue
    fi
    if ! git cat-file -e "${stamp}^{commit}" 2>/dev/null; then
        echo "ERROR $file: verified commit '$stamp' does not exist" >&2
        errors=$((errors + 1))
        continue
    fi
    if ! git merge-base --is-ancestor "$stamp" HEAD; then
        echo "ERROR $file: verified commit '$stamp' is not an ancestor of HEAD" >&2
        errors=$((errors + 1))
        continue
    fi

    stamped=$((stamped + 1))
    age=$(git rev-list --count "$stamp"..HEAD)
    if [ "$age" -gt "$max_commits" ]; then
        printf 'STALE  %s: %s commits behind HEAD (limit %s)\n' "$file" "$age" "$max_commits"
        stale=$((stale + 1))
    else
        printf 'OK     %s: %s commits behind HEAD\n' "$file" "$age"
    fi
done

printf 'spec freshness: %s stamped, %s unstamped, %s stale, %s invalid\n' \
    "$stamped" "$unstamped" "$stale" "$errors"

if [ "$errors" -ne 0 ]; then
    exit 1
fi
if [ "$strict" -eq 1 ] && [ "$stale" -ne 0 ]; then
    echo "check-spec-freshness: strict age limit exceeded" >&2
    exit 1
fi
if [ "$stale" -ne 0 ]; then
    echo "check-spec-freshness: age is advisory; review stale docs or rerun with --strict" >&2
fi
