#!/usr/bin/env bash
# Report source-file size hotspots without invoking Cargo.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

warn_lines=${WITCHY_WARN_SOURCE_LINES:-3000}
max_lines=${WITCHY_MAX_SOURCE_LINES:-12000}
top_n=${WITCHY_STRUCTURE_TOP:-12}
strict=0
json=0

usage() {
    cat <<'EOF'
usage: scripts/structure-health.sh [--json] [--strict]

Reports tracked Rust, shell, Witchy, JavaScript, and TypeScript source sizes.
WITCHY_WARN_SOURCE_LINES and WITCHY_MAX_SOURCE_LINES tune the thresholds.
EOF
}

for arg in "$@"; do
    case "$arg" in
        --json) json=1 ;;
        --strict) strict=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "structure-health.sh: unknown argument '$arg'" >&2; usage >&2; exit 2 ;;
    esac
done

case "$warn_lines:$max_lines:$top_n" in
    *[!0-9:]*|*:|*::*) echo "structure-health.sh: thresholds must be positive integers" >&2; exit 2 ;;
esac
if [ "$warn_lines" -le 0 ] || [ "$max_lines" -le "$warn_lines" ] || [ "$top_n" -le 0 ]; then
    echo "structure-health.sh: require 0 < warn < max and top > 0" >&2
    exit 2
fi

tmp=$(mktemp "${TMPDIR:-/tmp}/witchy-structure.XXXXXX")
trap 'rm -f "$tmp"' EXIT

while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file" | tr -d ' ')
    printf '%s\t%s\n' "$lines" "$file" >>"$tmp"
done < <(git ls-files -z -- '*.rs' '*.sh' '*.witchy' '*.js' '*.ts')

sorted=$(sort -nr -k1,1 "$tmp")
warnings=$(awk -F '\t' -v threshold="$warn_lines" '$1 >= threshold { count++ } END { print count + 0 }' "$tmp")
violations=$(awk -F '\t' -v threshold="$max_lines" '$1 > threshold { count++ } END { print count + 0 }' "$tmp")

if [ "$json" -eq 1 ]; then
    printf '{"warn_lines":%s,"max_lines":%s,"warning_count":%s,"violation_count":%s,"top":[' "$warn_lines" "$max_lines" "$warnings" "$violations"
    printf '%s\n' "$sorted" | head -n "$top_n" | awk -F '\t' '{
        if (n++) printf ",";
        gsub(/\\/, "\\\\", $2); gsub(/"/, "\\\"", $2);
        printf "{\"lines\":%s,\"file\":\"%s\"}", $1, $2
    }'
    printf ']}\n'
else
    printf 'source structure: %s warnings, %s hard-limit violations (warn >= %s, max > %s)\n' \
        "$warnings" "$violations" "$warn_lines" "$max_lines"
    printf '%s\n' "$sorted" | head -n "$top_n" | awk -F '\t' '{ printf "%6s %s\n", $1, $2 }'
fi

if [ "$violations" -gt 0 ] || { [ "$strict" -eq 1 ] && [ "$warnings" -gt 0 ]; }; then
    exit 1
fi
