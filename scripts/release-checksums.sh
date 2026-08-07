#!/usr/bin/env bash
# Write a fail-closed SHA256SUMS manifest for exact release archive bytes.
set -euo pipefail

if [ "$#" -lt 3 ] || [ "$1" != "--output" ]; then
    echo "usage: release-checksums.sh --output <.../SHA256SUMS> <archive>..." >&2
    exit 2
fi
output="$2"
shift 2
[ "$(basename "$output")" = "SHA256SUMS" ] || {
    echo "release-checksums: output must be named SHA256SUMS" >&2
    exit 1
}

python3 - "$output" "$@" <<'PY'
import hashlib
import os
import pathlib
import re
import sys
import tempfile

output = pathlib.Path(sys.argv[1])
archives = [pathlib.Path(path) for path in sys.argv[2:]]
pattern = re.compile(
    r"witchy-(\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.]*)?)"
    r"-(x86_64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin)\.tar\.gz"
)
if not archives:
    raise SystemExit("release-checksums: no archives supplied")
by_name = {}
versions = set()
for archive in archives:
    if archive.is_symlink() or not archive.is_file():
        raise SystemExit(f"release-checksums: archive is not a regular file: {archive}")
    match = pattern.fullmatch(archive.name)
    if not match:
        raise SystemExit(f"release-checksums: unexpected archive name: {archive.name}")
    versions.add(match.group(1))
    if archive.name in by_name:
        raise SystemExit(f"release-checksums: duplicate archive name: {archive.name}")
    by_name[archive.name] = archive
if len(versions) > 1:
    raise SystemExit(f"release-checksums: archives span multiple versions: {sorted(versions)}")

lines = []
for name, archive in sorted(by_name.items()):
    digest = hashlib.sha256()
    with archive.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    lines.append(f"{digest.hexdigest()}  {name}\n")

output.parent.mkdir(parents=True, exist_ok=True)
fd, temporary_name = tempfile.mkstemp(prefix=f".{output.name}.", dir=output.parent)
try:
    with os.fdopen(fd, "w", encoding="ascii", newline="\n") as handle:
        handle.writelines(lines)
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temporary_name, 0o644)
    os.replace(temporary_name, output)
finally:
    try:
        os.unlink(temporary_name)
    except FileNotFoundError:
        pass
print(output)
PY
