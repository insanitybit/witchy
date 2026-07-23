# Installation

## The zero-install option: the playground

The witchy compiler runs in WebAssembly, so the playground needs no server-side
compiler. If the repository ships a built playground, open `web/index.html`
(served over HTTP); otherwise build it once:

```sh
git clone https://github.com/insanitybit/witchy
cd witchy
./scripts/build-playground.sh
python3 -m http.server -d web 8000
# open http://localhost:8000
```

Examples with a Run button compile in the trusted page and execute in a fresh
opaque-origin frame whose CSP is derived from the example's browser grants.
Native-only host services and examples that require ungranted authority remain
read-only.

## Installing a tagged release

Tagged releases publish a `witchy` binary, the README and licenses in one
archive, plus a `SHA256SUMS` manifest. Choose the archive for your machine:

- Linux x86-64: `witchy-x86_64-unknown-linux-gnu.tar.gz`
- Apple Silicon: `witchy-aarch64-apple-darwin.tar.gz`
- Intel macOS: `witchy-x86_64-apple-darwin.tar.gz`

The following example uses the GitHub CLI and the Apple Silicon artifact.
Replace the tag and artifact as needed:

```sh
tag=v0.1.0
artifact=witchy-aarch64-apple-darwin.tar.gz

gh release download "$tag" --repo insanitybit/witchy \
  --pattern "$artifact" --pattern SHA256SUMS
awk -v file="$artifact" '$2 == file' SHA256SUMS > "$artifact.sha256"
test -s "$artifact.sha256"

# macOS:
shasum -a 256 --check "$artifact.sha256"
# Linux (use this instead of shasum):
# sha256sum --check "$artifact.sha256"

mkdir witchy-release
tar -xzf "$artifact" -C witchy-release
./witchy-release/witchy --version
```

The checksum proves that the archive matches the manifest attached to that
release. Because the manifest is not independently signed, it detects download
corruption or mismatched assets but does not provide a separate trust root from
GitHub.

## Building from source

To use witchy as a real tool — the sandbox, the package manager — build the
binary. This requires a Rust toolchain (`rustup` recommended):

```sh
git clone https://github.com/insanitybit/witchy
cd witchy
cargo build --release
```

That produces `./target/release/witchy`. Put it on your `PATH` (or call it by
path); the rest of this book writes just `witchy`.

```sh
witchy --help
```

## Editor support

There's a [Zed extension](https://github.com/insanitybit/witchy/tree/master/editors/zed)
with tree-sitter highlighting and a language server (`witchy lsp`) that provides
diagnostics, completion, and hover. Any editor that speaks LSP can use the
server.

The next chapter uses the browser or the installed binary to run a first program.
