# minigrep

The CLI search tool from chapter 12 of *The Rust Programming Language*, in
witchy. It reads a file with a read-only `Dir` capability, prints the lines that
contain a query, and switches to case-insensitive search when `IGNORE_CASE` is
set — with witchy's capability-typed entry point (`Console`, a read-only `Dir`,
`Env`, and the args). The two `search` functions are pure (`pub`).

**Shows:** a capability-typed `main`, `Dir[Read]`/`Env`/args, `match` on an
`Option`, `pub` functions across modules, in-rune `test_*` functions, and a
trusted standalone application whose read-only root is the launch cwd.

## Run

```sh
# from the repo root (the read-only Dir is the first argument):
witchy examples/minigrep/src/minigrep.witchy nobody examples/data/poem.txt
IGNORE_CASE=1 witchy examples/minigrep/src/minigrep.witchy BODY examples/data/poem.txt
```

## Build and install a trusted application

The manifest binds `main`'s `root: Dir[Read]` to the directory from which the
installed command is launched. Build one native executable containing the
compiled WASM and Witchy runtime:

```sh
witchy --release build --target trusted-exe examples/minigrep
install -m 755 examples/minigrep/target/release/minigrep ~/.local/bin/minigrep
```

The installed command needs neither Witchy nor capability flags:

```sh
cd /path/to/search-root
minigrep nobody poem.txt
```

Installing this artifact means trusting the application, its embedded Witchy
runtime, and its distributor. Its dependencies still receive no filesystem
authority unless `minigrep` explicitly passes them the `Dir[Read]`. Use the
portable WASM target and consumer-provided grants instead when the application
is not trusted.

## Test

```sh
witchy test examples/minigrep
```
