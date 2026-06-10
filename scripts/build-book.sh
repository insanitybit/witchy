#!/usr/bin/env bash
# Build "The witchy Book" with mdBook. Output goes to ./book-html (NOT ./book —
# that's the source, and mdBook cleans its output dir before rendering).
#
#   ./scripts/build-book.sh          # build
#   ./scripts/build-book.sh --serve  # build + live-reload server
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v mdbook >/dev/null; then
    echo "mdbook is not installed. Install it with:" >&2
    echo "    cargo install mdbook" >&2
    exit 1
fi

if [ "${1:-}" = "--serve" ]; then
    exec mdbook serve --open
fi

mdbook build
echo
echo "wrote the book to ./book-html — open ./book-html/index.html"
