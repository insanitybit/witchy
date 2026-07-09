# BUG-340: std/meta docs (and generated spec/stdlib.md) spell the comptime type list as a call `module_types()`, but it is injected as a let-bound variable — the documented spelling fails with `unknown function`

Severity: LOW
Status: FIXED
Verified: 2026-07-08 SOURCE on master a8cc2cc
Component: std/meta.witchy, generated spec/stdlib.md, crates/witchy-interp/src/comptime.rs, docs

## Problem

Current source and generated docs now spell the injected type list as the
`module_types` value, matching the comptime injection path.

## Historical Problem

At filing time, `std/meta.witchy:1-6` still documented the wrong spelling. It
said the compiler injects the type list as `module_types()`, but
`crates/witchy-interp/src/comptime.rs:52-108` still injects `module_types` as a
let-bound value, not a zero-argument function.

No runtime repro was rerun in this pass because another agent is actively
repairing the worktree/toolchain. This is a source-level verification against
the current docs and comptime injection path; the historical repro below remains
the expected user-visible symptom.

`spec/stdlib.md:1460` (generated from `std/meta.witchy:5`): "The compiler injects
the type list as `module_types()`" — i.e. the documented spelling is a zero-arg
CALL, in a doc file that otherwise uses `()` only for callables. But the compiler
injects `module_types` as a `Stmt::Let` binding a list VALUE, not a function, so
only bare `module_types` resolves.

A user following the meta docs to write a comptime generator with `for t in
module_types():` gets "link error: … comptime block: type error: `main`, line 9:
call to unknown function `module_types`" (exit 1) with no hint that the
bare-variable spelling works. The identical program with `for t in module_types:`
(no parens) passes parity and prints the types. No RFC mentions module_types, so
this is not a documented scope punt; the spec still promises the call spelling.

LOW: loud check-time error, not silent wrong behavior or parity divergence.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-comptime/t_ct_module_types.witchy
link error: module `t_ct_module_types`: comptime block: type error: `main`, line 9: call to unknown function `module_types`
# control: t_ct_module_types_var.witchy (for t in module_types:) → parity agree, prints Point/record
```

Probe: `/Users/cobrien/workspace/witchy/scratch/ultra-comptime/t_ct_module_types.witchy`;
control `t_ct_module_types_var.witchy`.

## Code evidence

- Filing-time `std/meta.witchy:5` — the doc-comment ("injects the type list as
  `module_types()`"), source of the generated `spec/stdlib.md:1460`.
- `crates/witchy-interp/src/comptime.rs:95-104` — injects `module_types` as a
  `Stmt::Let` binding a list VALUE, not a function, so only bare `module_types`
  resolves.
- No runnable example of module_types anywhere in book/, examples/, or src/ tests
  (grep found only std/meta.witchy), so the drift is invisible to the doc-test
  gates.
- Distinct from BUG-180 (comptime can't see same-module functions — module_types
  would remain uncallable even with that fixed).

## Fix direction

One-word doc edit: change `std/meta.witchy:5` to say "injects the list
`module_types`" (drop the parens), then regenerate `spec/stdlib.md` via `witchy
doc std/*.witchy > spec/stdlib.md`. Alternatively make the injection callable (a
zero-arg fn) to match the doc — but the variable form is simpler. Add a runnable
book/example that iterates `module_types` so the doc-test gate catches future
drift.
