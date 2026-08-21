# patterns

Exercises the standard library's regular-expression matcher (`std/regex`) over a
handful of patterns. It supports `. * + ? ^ $` and literal characters. Data-only
(only `main` touches the `Console`), so it runs identically interpreted,
compiled, and inside the capability sandbox.

**Shows:** the `regex` module (`matches`), `if`/`else` expressions, string
building.

## Run

```sh
witchy run                                      # from this directory
witchy examples/patterns/src/patterns.witchy    # or by file, from the repo root
```
