#!/usr/bin/env bash
# Verify the one release version across the complete Cargo workspace.
set -euo pipefail
cd "$(dirname "$0")/.."

expected_version="0.1.0"
expected_tag="v${expected_version}"

if [ "$#" -ne 1 ] || [ "$1" != "$expected_tag" ]; then
    echo "release-version: expected exactly '$expected_tag'" >&2
    exit 1
fi

python3 - "$expected_version" Cargo.toml crates/*/Cargo.toml <<'PY'
import pathlib
import sys
import tomllib

expected = sys.argv[1]
manifests = [pathlib.Path(path) for path in sys.argv[2:]]
expected_packages = {
    "witchy",
    "witchy-caps",
    "witchy-interp",
    "witchy-lower",
    "witchy-runtime",
    "witchy-syntax",
    "witchy-types",
    "witchy-wir",
}
seen = {}
for manifest in manifests:
    with manifest.open("rb") as handle:
        package = tomllib.load(handle).get("package")
    if not isinstance(package, dict):
        continue
    name = package.get("name")
    version = package.get("version")
    if name in seen:
        raise SystemExit(f"release-version: duplicate workspace package {name!r}")
    seen[name] = (version, manifest)

if set(seen) != expected_packages:
    missing = sorted(expected_packages - set(seen))
    unexpected = sorted(set(seen) - expected_packages)
    raise SystemExit(
        f"release-version: workspace package set mismatch; missing={missing}, unexpected={unexpected}"
    )

wrong = [f"{name}={version} ({path})" for name, (version, path) in sorted(seen.items()) if version != expected]
if wrong:
    raise SystemExit("release-version: expected every package at " + expected + "; got " + ", ".join(wrong))

print(f"release-version: {len(seen)} workspace packages are {expected}")
PY
