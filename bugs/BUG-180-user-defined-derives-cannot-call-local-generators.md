# BUG-180: User-defined derives can call local generators

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-bug-180-comptime-local-helpers`
Component: `derive(...)`, `comptime`, user-extensible language surface

## Summary

Custom derives and ordinary `comptime:` blocks can now call pure same-module
helper functions.

The comptime evaluator still runs in a zero-capability synthetic module: it keeps
bundled std imports and deliberately does not resolve project sibling modules.
Within the enclosing module, however, it now builds a pruned helper closure from
the comptime block's references. That closure includes only reachable local
functions, local types/constructors, and local type aliases, so ordinary runtime
entry points that refer to later generated code are not pulled into the comptime
program.

This makes the advertised derive model real: an unknown `derive(Hello)` routes to
a local or imported `derive_hello(...)` generator and can produce source the same
way the built-in `std/meta` derives do.

## Evidence

Regression coverage:

- `example_tests::comptime_and_custom_derives_can_call_local_helpers_on_both_backends`
  covers both a direct `comptime:` block calling `make_source()` and a custom
  `derive(Hello)` calling local `derive_hello(t: TypeInfo)`, with generated
  output verified on the interpreter and compiled WASM backend.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy-interp --lib comptime -- --nocapture
CARGO_TARGET_DIR=target-codex cargo test -p witchy comptime_and_custom_derives_can_call_local_helpers_on_both_backends -- --nocapture
```

Result: both passed.

## Residuals

Comptime remains intentionally isolated from project sibling modules and host
capabilities. If Witchy wants cross-module compile-time helper libraries, that
needs an explicit import and capability story rather than implicit access to the
runtime dependency graph.
