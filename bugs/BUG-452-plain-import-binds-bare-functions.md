# BUG-452: plain `import` binds bare functions

Severity: HIGH
Status: FIXED
Verified: 2026-07-07
Fixed: 2026-07-07 (`fix/plain-import-no-bare-functions-clean`)

## Summary

RFC-0042 says `import X` keeps functions qualified and `from X import f`
creates an unqualified binding. The linker still had a broad fallback that
resolved `f(...)` through any plain imported module when exactly one import
exported `f`, making `from` imports partly cosmetic.

## Resolution

The linker now builds an explicit per-module function-binding table from
`from X import f`. Bare direct calls resolve only to same-module functions,
builtins, or that explicit from-import table. If a plain import is the likely
source, the diagnostic says to write `X.f(...)` or add `from X import f`.

Validation:
- `cargo test -p witchy-syntax plain_import_does_not_bind_bare_functions -- --nocapture`
- `cargo test -p witchy-syntax -- --nocapture`
- `cargo test all_std_modules_type_check -- --nocapture`
- CLI repro: `import lib; shown(1)` now rejects, while `from lib import shown`
  and `lib.shown(1)` both check.
