#!/usr/bin/env bash
# Install the pinned mr-boxington (mbx) build cache binary under state/mbx.
#
# mbx is a content-addressed compiler cache that sits under Cargo as a
# RUSTC_WRAPPER plus an in-process cache agent (no daemon). scripts/bin/cargo
# routes build subcommands through it; see that shim for the policy and
# scratch/build-cache-eval-2026-09-17.md for the measurements behind it.
#
# This installer is idempotent and quiet on success. It never fails the caller:
# the SessionStart hook runs it with `|| true`, and when the binary is absent
# the shim falls back to plain cargo. Nothing here touches ~/.cargo, mise, or
# rust-analyzer configuration (that is what `mbx setup` would do; we do not run
# it on a shared machine).
#
#   ./scripts/mbx-install.sh            # install the pinned version if missing
#   ./scripts/mbx-install.sh --check    # report status, exit 1 if not installed
#   ./scripts/mbx-install.sh --force    # re-download and re-verify
#
# Upgrading: bump MBX_VERSION below; the release's SHA256SUMS is downloaded and
# verified, nothing is pinned here. A new version gets its own store directory
# (scripts/bin/cargo keys the store on the version), so the first build after
# an upgrade is cold and the old store is dead weight to delete once no
# session still links the old binary. Every version installs side by side
# under state/mbx/<version>/, shared by all worktrees, and scripts/bin/cargo
# runs the version pinned by ITS checkout's copy of this file, so a checkout
# gets a new mbx together with the shim policy written for it. (mbx 1.18
# moves an unguarded `target/` under its managed root on the first build; a
# shared "current" binary would have handed it to shims that predate the
# guard.) state/mbx/bin/mbx is kept only for shims older than that rule: it is
# created when missing or dangling and never flipped to a newer version.
set -euo pipefail

MBX_VERSION="1.18.0"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# Install under the MAIN checkout's state/ so every worktree shares one binary
# and one store (see scripts/bin/cargo for the same resolution).
state_root="$repo_root"
if common="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
    state_root="$(dirname "$common")"
fi
state_dir="$state_root/state/mbx"
bin_dir="$state_dir/bin"
install_dir="$state_dir/$MBX_VERSION"
binary="$install_dir/mbx"
link="$bin_dir/mbx"

mode="install"
case "${1:-}" in
    --check) mode="check" ;;
    --force) mode="force" ;;
    "") ;;
    *) echo "usage: $0 [--check|--force]" >&2; exit 2 ;;
esac

installed() {
    [ -x "$binary" ] && [ -x "$link" ]
}

if [ "$mode" = "check" ]; then
    if installed; then
        echo "mbx $MBX_VERSION installed at $binary"
        exit 0
    fi
    echo "mbx $MBX_VERSION not installed (run ./scripts/mbx-install.sh)"
    exit 1
fi

if [ "$mode" = "install" ] && installed; then
    exit 0
fi

# Map the host to a release asset name.
os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Darwin/arm64) triple="aarch64-apple-darwin" ;;
    Darwin/x86_64) triple="x86_64-apple-darwin" ;;
    Linux/aarch64|Linux/arm64) triple="aarch64-unknown-linux-gnu" ;;
    Linux/x86_64) triple="x86_64-unknown-linux-gnu" ;;
    *) echo "mbx-install: unsupported host $os/$arch" >&2; exit 1 ;;
esac
asset="mbx-$triple.tar.gz"
base="https://github.com/jdx/mr-boxington/releases/download/v$MBX_VERSION"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/mbx-install-XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fetch() { # fetch <url> <dest>
    if command -v curl >/dev/null 2>&1 && curl -fsSL --retry 3 -o "$2" "$1" 2>/dev/null; then
        return 0
    fi
    # curl can be blocked in agent sandboxes; gh reaches releases through the
    # GitHub API and its own credentials.
    if command -v gh >/dev/null 2>&1; then
        local name; name="$(basename "$1")"
        gh release download "v$MBX_VERSION" -R jdx/mr-boxington -p "$name" -D "$(dirname "$2")" --clobber >/dev/null 2>&1 \
            && [ -f "$(dirname "$2")/$name" ] && mv "$(dirname "$2")/$name" "$2" && return 0
    fi
    return 1
}

fetch "$base/$asset" "$tmp/$asset" || { echo "mbx-install: could not download $asset" >&2; exit 1; }
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || { echo "mbx-install: could not download SHA256SUMS" >&2; exit 1; }

expected="$(awk -v a="$asset" '$2 == a || $2 == "*" a {print $1}' "$tmp/SHA256SUMS" | head -1)"
[ -n "$expected" ] || { echo "mbx-install: $asset missing from SHA256SUMS" >&2; exit 1; }
actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
[ "$actual" = "$expected" ] || {
    echo "mbx-install: checksum mismatch for $asset (expected $expected, got $actual)" >&2
    exit 1
}

mkdir -p "$tmp/x"
tar -xzf "$tmp/$asset" -C "$tmp/x"
extracted="$(find "$tmp/x" -type f -name mbx -perm -u+x | head -1)"
[ -n "$extracted" ] || { echo "mbx-install: archive has no mbx binary" >&2; exit 1; }

rm -rf "$install_dir"
mkdir -p "$install_dir" "$bin_dir"
cp "$extracted" "$binary"
chmod +x "$binary"
[ -x "$link" ] || ln -sfn "$binary" "$link"

"$binary" --version >/dev/null 2>&1 || { echo "mbx-install: installed binary does not run" >&2; exit 1; }
echo "mbx-install: installed mbx $MBX_VERSION at $binary"
