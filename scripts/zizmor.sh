#!/usr/bin/env bash
# Vendored zizmor wrapper. Downloads the pinned release on first use into
# scripts/.zizmor/, verifies sha256 and the GitHub build attestation, then
# execs it. No global install required.
set -euo pipefail

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

VERSION="1.26.1"

# sha256 of each upstream release tarball. Update alongside VERSION.
SHA_aarch64_apple_darwin="68ab2b37836bbd44f6cfffcc102b9ffffbc20c5d67d84293dafb63bd2775a1da"
SHA_x86_64_apple_darwin="2967414a561f8c1264121e8f723c3b5abcf3d1bf7ce5063114df99985dd75801"
SHA_aarch64_unknown_linux_gnu="711f5af366b299128f9a04b1470e37d990b41fbd21f14a1a4148d25004a83762"
SHA_x86_64_unknown_linux_gnu="8556289a64e7aaf2400cd516f61a471aa91c5902cc56ad96a82fd12f90c2ef73"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE_DIR="$SCRIPT_DIR/.zizmor/$VERSION"
BIN="$CACHE_DIR/zizmor"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  TARGET="aarch64-apple-darwin";         SHA="$SHA_aarch64_apple_darwin" ;;
  Darwin-x86_64) TARGET="x86_64-apple-darwin";          SHA="$SHA_x86_64_apple_darwin" ;;
  Linux-aarch64) TARGET="aarch64-unknown-linux-gnu";    SHA="$SHA_aarch64_unknown_linux_gnu" ;;
  Linux-x86_64)  TARGET="x86_64-unknown-linux-gnu";     SHA="$SHA_x86_64_unknown_linux_gnu" ;;
  *) echo "scripts/zizmor.sh: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

if [ ! -x "$BIN" ]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "scripts/zizmor.sh: 'gh' is required to verify the release attestation" >&2
    exit 1
  fi

  mkdir -p "$CACHE_DIR"
  TMPDIR_X="$(mktemp -d)"
  trap 'rm -rf "$TMPDIR_X"' EXIT
  ASSET="zizmor-${TARGET}.tar.gz"
  URL="https://github.com/zizmorcore/zizmor/releases/download/v${VERSION}/${ASSET}"
  echo "scripts/zizmor.sh: downloading ${ASSET} (v${VERSION})" >&2
  curl --proto '=https' --tlsv1.2 -fsSL -o "$TMPDIR_X/$ASSET" "$URL"

  ACTUAL="$(sha256 "$TMPDIR_X/$ASSET" | awk '{print $1}')"
  if [ "$ACTUAL" != "$SHA" ]; then
    echo "scripts/zizmor.sh: sha256 mismatch for $ASSET" >&2
    echo "  expected: $SHA" >&2
    echo "  actual:   $ACTUAL" >&2
    exit 1
  fi

  echo "scripts/zizmor.sh: verifying attestation for ${ASSET}" >&2
  if ! gh attestation verify "$TMPDIR_X/$ASSET" \
      --repo zizmorcore/zizmor \
      --signer-workflow zizmorcore/zizmor/.github/workflows/release-binaries.yml \
      >&2; then
    echo "scripts/zizmor.sh: attestation verification failed for $ASSET" >&2
    exit 1
  fi

  tar -xzf "$TMPDIR_X/$ASSET" -C "$CACHE_DIR"
  chmod +x "$BIN"
fi

exec "$BIN" "$@"
