# The witchy browser playground

A static page that runs the **actual witchy interpreter**, compiled to
WebAssembly, entirely client-side — no server, no backend.

```sh
./scripts/build-playground.sh        # compiles src/lib.rs -> web/witchy.wasm
python3 -m http.server -d web 8000   # any static file server works
# open http://localhost:8000
```

(It must be served over HTTP, not opened as a `file://` URL — browsers won't
`fetch` the `.wasm` from the filesystem.)

## How it works

- `src/lib.rs` is the witchy interpreter as a library (lexer → parser → type
  checker → linker → tree-walking interpreter) with the wasmtime sandbox, the
  package manager, and the LSP excluded, so it builds for
  `wasm32-unknown-unknown`. It exports a tiny C ABI: `witchy_alloc` /
  `witchy_free` for marshaling and `witchy_run(ptr, len)`.
- `playground.js` loads the module, sends the editor's source in, and renders
  the printed output or the parse/type/runtime error.
- `index.html` is the UI, preloaded with the language reference's examples.

Only `Console`-only programs produce output (the browser grants no `Dir`/`Net`/
`Clock`/`Env`); a program that asks for those type-checks but errors when it
tries to use them — which itself demonstrates the capability model.

`web/witchy.wasm` is a build artifact (gitignored); regenerate it with the
build script.
