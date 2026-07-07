# BUG-547: `module_types` omits types emitted by earlier `comptime:` blocks

- **Severity:** MED
- **Status:** FIXED
- **Fixed:** 2026-07-07
- **Verified:** 2026-07-07 CODE on branch `fix/comptime-module-types`
- **Component:** `comptime`, `std/meta`, compile-time reflection, generated types

## Summary

Witchy now uses a sequential expanded-module model for `module_types`.
Immediately before each `comptime:` block runs, the compiler rebuilds the
injected `module_types` value from the module's current item set. That means a
later block sees handwritten types plus types emitted by earlier blocks in the
same module.

This matches the language story that `comptime:` output is appended before type
checking and then behaves like ordinary source. Emitted `comptime:` blocks are
still rejected, so expansion remains additive without recursive compile-time
execution.

## Regression Coverage

- `module_types_include_types_emitted_by_earlier_comptime_blocks` emits a
  `Generated` record in one block, confirms a later block sees both `Generated`
  and `Handwritten` through `module_types`, and confirms ordinary checked code
  can construct and read the emitted record.

## Validation

- `cargo test -p witchy-interp module_types_include_types_emitted_by_earlier_comptime_blocks -- --nocapture`
