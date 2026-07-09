# BUG-443: source-spellable generated local names bypassed namespace reservation

Status: FIXED
Severity: HIGH
Verified: 2026-07-09
Fixed: 2026-07-09 (`fix/d5-reserve-compiler-namespace`)

## Summary

The linker already reserved user identifiers containing `__`, including
compiler-private names and method-lowering mangles. It also needed an exception
for parser-generated locals such as `__compr0`, `__range0`, `__fv0`, and
`__kw0`.

That exception was too broad: it looked only at the name shape, so user source
could write a generated-looking binding such as `let __compr0 = ...` and pass
the reserved-name gate.

## Resolution

Generated local-name exemptions now require synthetic source-line metadata
(`0` or `u32::MAX`). User-written statements carry real source lines and are
rejected even if their names match a generated prefix.

Loop counters need one extra split: `for var x in xs` desugars to a generated
`__fvN` loop binding while preserving the original source line on the outer
statement. The parser now rejects source-authored loop variables containing
`__`, and the linker keeps generated loop counters legal.

## Verification

- `CARGO_TARGET_DIR=target-codex-d5 cargo test -p witchy-syntax user_source_cannot_declare_compiler_private_names -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-d5 cargo test -p witchy-syntax private_bridge_intrinsics_are_std_only -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-d5 cargo test -p witchy-syntax -- --nocapture`
- `CARGO_TARGET_DIR=target-codex-d5 cargo test all_std_modules_type_check -- --nocapture`

The regression rejects source-written `__compr0`, `__fv0`, and `__fortuple0`
bindings while preserving parser-generated list-comprehension and `for var`
lowerings.
