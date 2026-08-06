#!/usr/bin/env bash
# Package or smoke the private docs preview as one archive. The smoke path
# extracts into a new directory and serves only those bytes to the browser
# confinement probe; repository web/book files cannot satisfy a missing asset.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  echo "usage: docs-artifact.sh package <bundle-dir> <archive.tar.gz>" >&2
  echo "       docs-artifact.sh smoke <archive.tar.gz> [safari|chrome]" >&2
  exit 2
}

case "${1:-}" in
  package)
    [ "$#" -eq 3 ] || usage
    bundle="$2"
    archive="$3"
    [ -d "$bundle" ] || {
      echo "docs artifact: bundle directory does not exist: $bundle" >&2
      exit 1
    }
    mkdir -p "$(dirname "$archive")"
    bundle_abs="$(cd "$bundle" && pwd -P)"
    archive_dir="$(cd "$(dirname "$archive")" && pwd -P)"
    archive_abs="$archive_dir/$(basename "$archive")"
    case "$archive_abs" in
      "$bundle_abs"/*)
        echo "docs artifact: archive must not be written inside its input bundle" >&2
        exit 1
        ;;
    esac
    rm -f "$archive_abs"
    tar -C "$bundle_abs" -czf "$archive_abs" .
    echo "packaged private docs artifact: $archive_abs"
    ;;
  smoke)
    [ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
    archive="$2"
    browser="${3:-safari}"
    [ -f "$archive" ] || {
      echo "docs artifact: archive does not exist: $archive" >&2
      exit 1
    }
    case "$browser" in
      safari|chrome) ;;
      *) usage ;;
    esac
    scratch="$(mktemp -d)"
    cleanup() {
      rm -rf "$scratch"
    }
    trap cleanup EXIT
    root="$scratch/extracted"
    mkdir -p "$root"
    while IFS= read -r member; do
      case "$member" in
        /*|../*|*/../*|*/..)
          echo "docs artifact: unsafe archive member: $member" >&2
          exit 1
          ;;
      esac
    done < <(tar -tzf "$archive")
    tar -xzf "$archive" -C "$root"
    node scripts/probe-browser-confinement.mjs "$root" "$browser"
    ;;
  *)
    usage
    ;;
esac
