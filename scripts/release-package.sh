#!/usr/bin/env bash
# Build one deterministic release archive from an already-built native binary.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
version="0.1.0"
binary=""
target=""
commit=""
output_dir=""
source_date_epoch="${SOURCE_DATE_EPOCH:-}"

usage() {
    echo "usage: release-package.sh --binary <.../witchy> --target <triple> --commit <40-hex-sha> --output-dir <dir> [--source-date-epoch <unix-seconds>]" >&2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary | --target | --commit | --output-dir | --source-date-epoch)
            [ "$#" -ge 2 ] || { usage; exit 2; }
            case "$1" in
                --binary) binary="$2" ;;
                --target) target="$2" ;;
                --commit) commit="$2" ;;
                --output-dir) output_dir="$2" ;;
                --source-date-epoch) source_date_epoch="$2" ;;
            esac
            shift 2
            ;;
        -h | --help) usage; exit 0 ;;
        *) echo "release-package: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done

[ -n "$binary" ] && [ -n "$target" ] && [ -n "$commit" ] && [ -n "$output_dir" ] || {
    usage
    exit 2
}

case "$target" in
    x86_64-unknown-linux-gnu | aarch64-apple-darwin | x86_64-apple-darwin) ;;
    *) echo "release-package: unsupported target '$target'" >&2; exit 1 ;;
esac
if [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "release-package: --commit must be exactly 40 lowercase hexadecimal characters" >&2
    exit 1
fi
case "$source_date_epoch" in
    "")
        source_date_epoch="$(git -C "$repo_root" show -s --format=%ct "$commit" 2>/dev/null || true)"
        ;;
    *[!0-9]*) echo "release-package: source date epoch must be a non-negative integer" >&2; exit 1 ;;
esac
[ -n "$source_date_epoch" ] || {
    echo "release-package: cannot derive SOURCE_DATE_EPOCH for commit $commit" >&2
    exit 1
}

[ "$(basename "$binary")" = "witchy" ] || {
    echo "release-package: input binary must be named exactly 'witchy'" >&2
    exit 1
}
[ -f "$binary" ] && [ -x "$binary" ] && [ ! -L "$binary" ] || {
    echo "release-package: input must be a regular executable file, not a symlink: $binary" >&2
    exit 1
}

expected="witchy ${version} (commit ${commit})"
actual="$($binary --version 2>&1)" || {
    echo "release-package: input binary cannot report its version" >&2
    exit 1
}
[ "$actual" = "$expected" ] || {
    echo "release-package: provenance mismatch" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
}

for file in README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE; do
    path="$repo_root/$file"
    [ -f "$path" ] && [ ! -L "$path" ] || {
        echo "release-package: required metadata is missing or is a symlink: $file" >&2
        exit 1
    }
done

mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd -P)"
archive="$output_dir/witchy-${version}-${target}.tar.gz"
root="witchy-${version}-${target}"

python3 - "$archive" "$root" "$source_date_epoch" "$binary" "$repo_root" <<'PY'
import gzip
import io
import os
import pathlib
import posixpath
import sys
import tarfile
import tempfile

archive = pathlib.Path(sys.argv[1])
root = sys.argv[2]
epoch = int(sys.argv[3])
binary = pathlib.Path(sys.argv[4])
repo = pathlib.Path(sys.argv[5])

files = [
    (f"{root}/bin/witchy", binary, 0o755),
    (f"{root}/README.md", repo / "README.md", 0o644),
    (f"{root}/CHANGELOG.md", repo / "CHANGELOG.md", 0o644),
    (f"{root}/LICENSE-MIT", repo / "LICENSE-MIT", 0o644),
    (f"{root}/LICENSE-APACHE", repo / "LICENSE-APACHE", 0o644),
]
directories = [root, f"{root}/bin"]

def safe_name(name: str) -> bool:
    return (
        name
        and not name.startswith("/")
        and "\\" not in name
        and posixpath.normpath(name) == name
        and all(part not in ("", ".", "..") for part in name.split("/"))
    )

for name in directories + [name for name, _, _ in files]:
    if not safe_name(name):
        raise SystemExit(f"release-package: unsafe archive member name {name!r}")
for _, source, _ in files:
    if source.is_symlink() or not source.is_file():
        raise SystemExit(f"release-package: source is not a regular file: {source}")

archive.parent.mkdir(parents=True, exist_ok=True)
fd, temporary_name = tempfile.mkstemp(prefix=f".{archive.name}.", dir=archive.parent)
os.close(fd)
temporary = pathlib.Path(temporary_name)
try:
    with temporary.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=raw, mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode="w", format=tarfile.GNU_FORMAT) as tar:
                for name in directories:
                    info = tarfile.TarInfo(name)
                    info.type = tarfile.DIRTYPE
                    info.mode = 0o755
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = epoch
                    tar.addfile(info)
                for name, source, mode in files:
                    data = source.read_bytes()
                    info = tarfile.TarInfo(name)
                    info.type = tarfile.REGTYPE
                    info.mode = mode
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = epoch
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
    os.chmod(temporary, 0o644)
    os.replace(temporary, archive)
finally:
    try:
        temporary.unlink()
    except FileNotFoundError:
        pass

expected = directories + [name for name, _, _ in files]
with tarfile.open(archive, mode="r:gz") as tar:
    members = tar.getmembers()
    names = [member.name for member in members]
    if names != expected:
        raise SystemExit(f"release-package: archive member mismatch: {names!r}")
    for member in members:
        if not safe_name(member.name) or member.issym() or member.islnk():
            raise SystemExit(f"release-package: unsafe archive member: {member.name!r}")
        if member.name in directories:
            if not member.isdir() or member.mode != 0o755:
                raise SystemExit(f"release-package: invalid directory metadata: {member.name}")
        else:
            expected_mode = 0o755 if member.name.endswith("/bin/witchy") else 0o644
            if not member.isfile() or member.mode != expected_mode:
                raise SystemExit(f"release-package: invalid file metadata: {member.name}")
PY

echo "$archive"
