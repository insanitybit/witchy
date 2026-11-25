#!/bin/sh
set -eu
script_dir=${0%/*}
[ "$script_dir" != "$0" ] || script_dir=.
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
version=4.36.0
if [ -n "${WITCHY_LEAN_BIN:-}" ]; then
    bin=$WITCHY_LEAN_BIN
else
    case "$(uname -sm)" in
        'Darwin arm64') platform=darwin_aarch64 ;;
        *) echo 'Set WITCHY_LEAN_BIN to the bin directory of Lean 4.36.0 for this platform.' >&2; exit 1 ;;
    esac
    bin="$root/.tools/lean-$version-$platform/bin"
    if [ ! -x "$bin/lean" ]; then
        mkdir -p "$root/.tools"
        archive=$(mktemp "$root/.tools/download.XXXXXX")
        trap 'rm -f "$archive"' EXIT HUP INT TERM
        curl -fL "https://github.com/leanprover/lean4/releases/download/v$version/lean-$version-$platform.tar.zst" -o "$archive" >&2
        expected=49acada8749ad1343eff1224a3d2510ac61ed53363d37b5e738e8c587e7deabd
        actual=$(shasum -a 256 "$archive" | cut -d ' ' -f 1)
        [ "$actual" = "$expected" ] || { echo 'Lean archive checksum mismatch' >&2; exit 1; }
        tar --zstd -xf "$archive" -C "$root/.tools"
    fi
fi
reported_version=$("$bin/lean" --version)
case "$reported_version" in
    *"version $version,"*) ;;
    *) echo 'Wrong Lean version' >&2; exit 1 ;;
esac
printf '%s\n' "$bin"
