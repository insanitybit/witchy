#!/usr/bin/env bash
# Install a prebuilt witchy release binary from GitHub releases.
#
#   curl --proto '=https' --tlsv1.2 -fsSL \
#     https://raw.githubusercontent.com/insanitybit/witchy/master/install.sh | bash
#
# Environment:
#   WITCHY_VERSION      release to install, e.g. 0.1.1 (default: latest)
#   WITCHY_INSTALL_DIR  destination directory (default: ~/.local/bin)
#
# The archive checksum is verified against the release's SHA256SUMS before
# extraction. Each archive also carries GitHub build provenance:
#   gh attestation verify --repo insanitybit/witchy <archive>
set -euo pipefail

repo="insanitybit/witchy"
install_dir="${WITCHY_INSTALL_DIR:-$HOME/.local/bin}"

fetch() {
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location "$@"
}

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
    Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
    Darwin/arm64) target="aarch64-apple-darwin" ;;
    Darwin/x86_64) target="x86_64-apple-darwin" ;;
    *)
        echo "install.sh: no prebuilt witchy binary for $os/$arch; build from source with 'cargo build --release'" >&2
        exit 1
        ;;
esac

version="${WITCHY_VERSION:-}"
if [ -z "$version" ]; then
    # The /releases/latest redirect target names the latest tag; no API token
    # or JSON parsing needed.
    latest_url="$(fetch --output /dev/null --write-out '%{url_effective}' "https://github.com/$repo/releases/latest")"
    version="${latest_url##*/tag/v}"
fi
version="${version#v}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$ ]]; then
    echo "install.sh: could not resolve a release version (got '$version')" >&2
    exit 1
fi

archive="witchy-$version-$target.tar.gz"
base="https://github.com/$repo/releases/download/v$version"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading witchy $version for $target"
fetch --output "$tmp/$archive" "$base/$archive"
fetch --output "$tmp/SHA256SUMS" "$base/SHA256SUMS"

expected="$(awk -v f="$archive" '$2 == f { print $1 }' "$tmp/SHA256SUMS")"
if [ -z "$expected" ]; then
    echo "install.sh: $archive is not listed in the release SHA256SUMS" >&2
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$archive" | awk '{ print $1 }')"
else
    actual="$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')"
fi
if [ "$actual" != "$expected" ]; then
    echo "install.sh: checksum mismatch for $archive" >&2
    echo "  expected $expected" >&2
    echo "  got      $actual" >&2
    exit 1
fi

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$install_dir"
install -m 0755 "$tmp/witchy-$version-$target/bin/witchy" "$install_dir/witchy"

echo "installed $install_dir/witchy ($("$install_dir/witchy" --version))"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "note: $install_dir is not on your PATH" ;;
esac
