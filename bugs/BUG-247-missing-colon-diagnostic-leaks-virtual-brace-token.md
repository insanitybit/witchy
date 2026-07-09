# BUG-247: Missing-colon diagnostic leaks virtual brace token

Severity: LOW
Status: FIXED
Verified: 2026-07-08 fixed on master 109e62a1
Fixed: 2026-07-08
Component: parser diagnostics, layout syntax, first-run UX

## Problem

When a block header omitted its trailing `:`, the parser reported `expected {`,
even though braces are not user-facing block syntax. This leaked the virtual
layout representation and pointed users at the wrong fix.

For a public release, common syntax mistakes should name the source syntax:
missing colon / expected indented block, not an implementation token.

## Resolution

`Parser::expect(Tok::LBrace)` now reports the layout-level expectation:

```text
parse error at 2:5: expected an indented block; add `:` after the header (found `print`)
```

Regression coverage:

- `diagnostic_golden_tests::parse::missing_header_colon`

## Reproduction

`scratch/repro-missing-colon-diagnostic.witchy`:

```witchy
fn main(console: Console)
    print(console, "hi")
```

Current output:

```console
$ ./target/debug/witchy check scratch/repro-missing-colon-diagnostic.witchy
repro-missing-colon-diagnostic: parse error at 2:5: expected an indented block; add `:` after the header (found `print`)
```

## Evidence

- `crates/witchy-syntax/src/lexer.rs:946-950` documents that the layout pass
  turns indentation into brace-delimited blocks before parsing.
- `crates/witchy-syntax/src/parser.rs:595`, `:1047`, and `:1928` consume
  `Tok::LBrace` directly for block bodies.
- `crates/witchy-syntax/src/parser.rs:19` formats raw parser messages without a
  source-syntax translation layer, so `Tok::LBrace` appears as `{`.

## Expected fix

Map virtual-layout token expectations back to user syntax in parse errors. For
this common case, something like:

```text
expected an indented block; add `:` after the header
```

would be much more useful than `expected {`.

## Acceptance

- The repro diagnostic mentions the missing `:` / expected indented block.
- User-facing parser errors do not ask layout-syntax users to type `{` for an
  ordinary block.
- Explicit brace-mode tests, if still supported internally, keep their own
  diagnostics separate from layout-source diagnostics.
