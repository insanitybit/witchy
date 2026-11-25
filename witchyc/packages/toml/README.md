# TOML for Witchy

This package reads TOML 1.0 and 1.1. It takes an owned string and an
explicit allocator. File and network I/O stay with the caller. A branded
handle can read only the document it came from.

Build the versioned payload from the monorepo root.

```sh
grimoire build witchyc/packages/toml
```

For a local dependency, put a line like this next to your executable entry,
in `main.packages` for `main.wy`. Use a source path relative to that file.

```text
toml 0.1.0: path/to/witchyc/packages/toml/toml.wy ; requires
```

Import the library with `import "@toml"`. See the
[parser API, examples, and limits](../../docs/wasm-toml.md).
