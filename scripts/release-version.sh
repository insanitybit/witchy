#!/usr/bin/env bash
# Verify the one release version across the complete Cargo workspace.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -ne 1 ]; then
    echo "usage: release-version.sh vX.Y.Z" >&2
    exit 2
fi
tag="$1"
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$ ]]; then
    echo "release-version: tag must look like vX.Y.Z or vX.Y.Z-pre; got '$tag'" >&2
    exit 1
fi
expected_version="${tag#v}"

python3 - "$expected_version" Cargo.toml crates/*/Cargo.toml <<'PY'
import pathlib
import sys
import tomllib

expected = sys.argv[1]
manifests = [pathlib.Path(path) for path in sys.argv[2:]]
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

if "witchy" not in seen:
    raise SystemExit("release-version: the 'witchy' binary package is missing from the workspace")
if len(seen) < 2:
    raise SystemExit("release-version: found no workspace crates beside the root package")

wrong = [f"{name}={version} ({path})" for name, (version, path) in sorted(seen.items()) if version != expected]
if wrong:
    raise SystemExit("release-version: expected every package at " + expected + "; got " + ", ".join(wrong))

print(f"release-version: {len(seen)} workspace packages are {expected}")
PY
