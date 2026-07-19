# BUG-331: Formatter dropped or refused same-line comments

Status: FIXED
Severity: HIGH
Component: `witchy-syntax` formatter/comment trivia

## Summary

`witchy fmt` only preserved own-line comments. A previous safety guard avoided
silent deletion by refusing files with trailing `//` comments or inline
`/* ... */` block comments, but that still meant common source files could not
participate in the formatter-driven 0.1 modernization sweeps.

## Resolution

The lexer now records same-line comments separately from own-line comments.
The formatter anchors those comments to their source statement line and appends
them to the formatted statement. If a same-line comment has no supported anchor,
`reformat` still refuses rather than dropping it.

Covered by:

```sh
CARGO_TARGET_DIR=target-codex-d8 cargo test -p witchy-syntax preserves_trailing_and_inline_comments
CARGO_TARGET_DIR=target-codex-d8 cargo test --test misc fmt_cli::fmt_preserves_trailing_comments
CARGO_TARGET_DIR=target-codex-d8 cargo test -p witchy-syntax reformats_every_std_and_example_to_an_equal_ast
```

Also verified with `./scripts/check.sh --fast`.
