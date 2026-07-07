# BUG-525: Comptime-emitted `from` imports are dropped

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: `comptime`, generated-source imports, RFC-0042 module bindings

## Summary

`comptime:` output now preserves `from X import Y` bindings when the emitted
module fragment is merged back into the enclosing module. Plain emitted imports
were already retained; the missing `from_imports` merge meant generated code
could parse a correct import declaration and still fail later when using the
bare imported name.

The merge combines bindings for the same source module and avoids duplicating
the same imported name. Conflicting unqualified names are still diagnosed by the
ordinary RFC-0042 resolver.

## Regression Coverage

- `emitted_from_imports_bind_generated_items` emits `from json import Json` and
  a generated public function whose parameter uses bare `Json`; linking and
  type checking now succeed.

## Validation

- `cargo test -p witchy-interp emitted_from_imports_bind_generated_items -- --nocapture`
