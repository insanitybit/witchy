# BUG-162: LSP linker diagnostics point at line zero

Severity: MED
Status: FIXED
Verified: 2026-07-09 — `string.not_real(...)` now carries its parsed module and statement line through `LinkError`; the LSP diagnostic starts on source line 5
Component: LSP diagnostics, linker errors, editor/CLI diagnostic parity
Discovered: 2026-07-05

## Summary

`witchy lsp` publishes linker diagnostics against the first line of the open
buffer, even when the offending construct is later in the file. This makes
editor feedback look arbitrary for module/function-resolution mistakes.

This is separate from BUG-137, which tracks skipped parse errors in imported
sibling files. Here the linker error is reported, but its range is wrong.

## Reproduction

`scratch/repro-lsp-link-line0.witchy`:

```witchy
import string

fn main(console: Console):
    print(console, "before")
    print(console, string.not_real("x"))
```

CLI check:

```console
$ ./target/debug/witchy check scratch/repro-lsp-link-line0.witchy
link error: module `string` has no function `not_real`
```

LSP probe:

```console
$ python3 <stdio LSP probe opening scratch/repro-lsp-link-line0.witchy>
diagnostics= [{"message": "link error: module `string` has no function `not_real`", "range": {"end": {"character": 13, "line": 0}, "start": {"character": 0, "line": 0}}, "severity": 1, "source": "witchy"}]
```

The invalid call is on source line 5, but the published diagnostic range starts
on LSP line `0`, the `import string` line.

## Code Evidence

- `src/lsp.rs:326-328` maps every `crate::pipeline::link(...)` failure through
  `line_diag(0, text, &e.to_string())`, regardless of the error cause or source
  location.
- `src/lsp.rs:357-360` has a separate type-error path that extracts `line N`
  from checker messages; the unknown-call repro on line 3 takes that path and
  reports the correct LSP line, so the line-zero behavior is specific to linker
  diagnostics.

## Expected Behavior

For release-facing editor polish, linker diagnostics should be anchored to the
nearest source construct when possible:

- `module X has no function Y` should point at the `X.Y(...)` call or value
  reference;
- missing-import diagnostics should point at the qualified call or the relevant
  import area;
- module-level structural errors can fall back to line 0 only when no better
  span exists.

## Impact

The language increasingly relies on module-qualified APIs and associated
constructors. If the editor points linker mistakes at line 1, users have to
mentally search for the real failing expression, which makes the toolchain feel
less cohesive than the CLI/typechecker paths.

## Resolution

`LinkError` now carries an optional structured `LinkLocation { module, line }`.
The linker threads the parser's existing statement lines through qualified call
resolution, including calls nested inside larger expressions. The LSP consumes
that location directly when it belongs to the current module; an error from an
imported module anchors to that module's import instead of applying a foreign
line number to the open buffer. Message text is no longer parsed or searched to
locate qualified calls.

Coverage includes the full diagnostic path for a failing
`string.not_real("x")` call on line 5, the location helper's imported-module
fallback, all LSP tests, all linker tests, and the fast workspace gate.

## Suggested Fix

Carry source spans through linker errors instead of formatting them as plain
strings only. A small intermediate improvement would be to recover the qualified
callee from common linker messages and search the open buffer for that spelling,
but the stronger fix is a structured `LinkError { message, span }` or equivalent
diagnostic object shared by CLI and LSP.

## Acceptance Criteria

- LSP diagnostics for `string.not_real(...)` point at the source line containing
  `string.not_real`, not line 0.
- Existing parse/type diagnostics continue to point at their current lines.
- Tests cover at least one linker failure in `src/lsp_tests.rs`.
