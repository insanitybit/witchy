# regex

A tiny regular-expression matcher in the style of Kernighan & Pike. It supports
literal characters, `.` (any one character), `*` (zero or more of the preceding
character), and the `^` / `$` anchors. The match is recursive: `match_here`
consumes pattern and text together while `match_star` handles the zero-or-more
repetition. The matcher is data-only (`pub`, no capabilities); only `main` touches the
`Console`, so it runs identically interpreted, compiled, and inside the
capability sandbox.

**Shows:** recursion, `List(String)` character scanning, tuples, `pub` functions
across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/regex/src/regex_demo.witchy # or by file, from the repo root
```

## Test

```sh
witchy test examples/regex
```
