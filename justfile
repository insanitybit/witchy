# Public Witchy tasks.
set shell := ["bash", "-uc"]

build:
    cargo build --workspace

build-release:
    cargo build --release

test:
    cargo test --workspace

book:
    cargo build --release
    ./scripts/build-docs.sh --allow-missing-compiler dist

docs-build:
    cargo build --release
    ./scripts/build-docs.sh dist

book-serve: docs-build
    python3 -m http.server -d dist 8000

clean:
    cargo clean
