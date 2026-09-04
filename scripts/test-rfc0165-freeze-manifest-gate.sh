#!/usr/bin/env bash
# Focused A10 manifest-gate test. It exercises only the manifest contract.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d "${TMPDIR:-/tmp}/witchy-rfc0165-manifest.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/examples"
printf '%s\n' 'tiny-invalid' >"$tmp/examples/tiny_invalid.witchy"
printf '%s\n' 'tiny-valid' >"$tmp/examples/tiny_valid.witchy"

manifest="$tmp/manifest.json"
cp perf/rfc0165-freeze-manifest.fixture.json "$manifest"
gate="scripts/rfc0165-freeze-manifest-gate.sh"
check() {
    bash "$gate" --root "$tmp" --manifest "$manifest" >/dev/null
}
check

mutate_and_reject() {
    local label="$1"
    local expression="$2"
    cp perf/rfc0165-freeze-manifest.fixture.json "$manifest"
    python3 - "$manifest" "$expression" <<'PY'
import json
import sys
path, expression = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    manifest = json.load(handle)
exec(expression, {"manifest": manifest})
with open(path, "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2)
    handle.write("\n")
PY
    if bash "$gate" --root "$tmp" --manifest "$manifest" >/dev/null 2>"$tmp/error"; then
        echo "test-rfc0165-freeze-manifest-gate: $label unexpectedly accepted" >&2
        exit 1
    fi
}

mutate_and_reject "added source" \
    'manifest["entries"].append({"source":"examples/added.witchy"})'
mutate_and_reject "removed source" \
    'manifest["entries"].pop()'
mutate_and_reject "renamed source" \
    'manifest["entries"][0]["source"]="examples/renamed.witchy"'
mutate_and_reject "source digest tampering" \
    'manifest["entries"][0]["source_sha256"]="0"*64'
mutate_and_reject "trace digest tampering" \
    'manifest["entries"][0]["profiles"]["normal"]["observation"]["digest"]="0"*64'
mutate_and_reject "snapshot digest tampering" \
    'manifest["snapshot_digest"]="0"*64'

echo "test-rfc0165-freeze-manifest-gate: ok (sorted universe, profiles, source and trace immutability)"
