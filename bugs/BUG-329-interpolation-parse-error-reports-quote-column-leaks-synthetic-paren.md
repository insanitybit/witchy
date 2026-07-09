# BUG-329: Plain-string `${…}` parse errors report the literal's opening-quote column and leak the synthetic desugar token `)`

Severity: LOW
Status: FIXED
Verified: 2026-07-08 fixed on master 109e62a1
Fixed: 2026-07-08
Component: crates/witchy-syntax/src/lexer.rs, RFC-0006 hole-precise diagnostics, diagnostics

## Problem

This row is stale. The compiler's diagnostics design says interpolation-hole
errors should "point INTO the literal at that `${…}`". That is now true for
plain string interpolation parse errors as well as tagged literals.

Current behavior for
`print(console, "a=${a} then b=${b + } then c=${c}")` reports a column inside
the broken hole and displays the user-visible interpolation close token:

```text
interp: parse error at 5:41: expected an expression, found `}`
```

Regression coverage:

- `diagnostic_golden_tests::parse::interpolation_hole_parse_error`

LOW: correct rejection with correct line, only column and token spelling mislead.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ D=/Users/cobrien/workspace/witchy/scratch/ultra-diag
$ $W check $D/t_interp_pos.witchy
t_interp_pos: parse error at 5:20: expected an expression, found `)`     # 5:20 is the opening quote
$ $W check $D/t_interp_pos2.witchy                                        # hole at col 58 → still 5:20

# control: real user parens report the exact column:
$ $W check $D/t_paren_ctl.witchy   # let b = (a + )  → parse error at 3:18 (the user's `)`)
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-diag/t_interp_pos.witchy`,
`t_interp_pos2.witchy`; control `t_paren_ctl.witchy`.

## Code evidence

- `crates/witchy-syntax/src/lexer.rs:591-594` — documents the desugar
  (`( lit0 + __render(expr0) + lit1 + … )`).
- `crates/witchy-syntax/src/lexer.rs:645-647` — the synthetic trailing
  `Tok::RParen` is what "found `)`" names.
- `crates/witchy-syntax/src/lexer.rs:687-695` — hole start positions are captured
  for tagged literals only (`tag_literal` records `(self.line, self.col)` per
  hole); `string()`/`emit_interpolation` records none, so the parser can only
  attribute to the literal-start position.
- Distinct from BUG-247 (layout pass leaking virtual `{` — different component,
  root cause, fix).

## Fix direction

Closed by the current interpolation-token span behavior and
`diagnostic_golden_tests::parse::interpolation_hole_parse_error`.
