# minigrep

The CLI search tool from chapter 12 of *The Rust Programming Language*, in
witchy. It reads a file with a read-only `Dir` capability, prints the lines that
contain a query, and switches to case-insensitive search when `IGNORE_CASE` is
set — with witchy's capability-typed entry point (`Console`, a read-only `Dir`,
`Env`, and the args). The two `search` functions are pure (`pub`).

**Shows:** a capability-typed `main`, `Dir[Read]`/`Env`/args, `match` on an
`Option`, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
# from the repo root (the read-only Dir is the first argument):
witchy examples/minigrep/src/minigrep.witchy nobody examples/data/poem.txt
IGNORE_CASE=1 witchy examples/minigrep/src/minigrep.witchy BODY examples/data/poem.txt
```

## Test

```sh
witchy test examples/minigrep
```
