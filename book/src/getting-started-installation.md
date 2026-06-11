# Installation

## The zero-install option: the playground

The witchy interpreter compiles to WebAssembly, so it runs in a browser with no
backend. If the repository ships a built playground, open `web/index.html`
(served over HTTP); otherwise build it once:

```sh
git clone https://github.com/insanitybit/witchy
cd witchy
./scripts/build-playground.sh
python3 -m http.server -d web 8000
# open http://localhost:8000
```

Every `Console`-only example in this book runs there. It's the quickest way to
follow along.

## Building from source

To use witchy as a real tool — the sandbox, the package manager — build the
binary. You'll need a Rust toolchain (`rustup`
recommended):

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

With that, let's write something.
