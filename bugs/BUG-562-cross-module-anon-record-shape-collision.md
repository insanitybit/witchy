# BUG-562: Cross-module anonymous-record shapes collided under local synthetic names

Status: FIXED
Severity: HIGH
Component: `witchy-syntax` parser/type resolution, anonymous records

## Summary

Anonymous record literals (`.{...}`) used to synthesize per-module local type
names (`__anon0`, `__anon1`, ...). Those names intentionally stay bare through
type resolution because they are compiler-private records, not user module
types. When two linked modules each used their first anonymous record for
different field sets, both emitted a bare `__anon0` type and the merged checker
rejected the program as a duplicate type definition.

The old behavior made anonymous records unreliable across module boundaries and
made import order/first-use order part of the type identity.

## Repro

```witchy
# left.witchy
pub fn make():
    .{a: 1}

# main.witchy
import left

fn main(console: Console):
    let _ = left.make()
    let local = .{b: 2}
    print(console, "${local.b}")
```

Before the fix, `witchy check main.witchy` failed with:

```text
type `__anon0` is defined more than once; top-level type names must be unique
```

## Resolution

Anonymous-record synthetic type names are now stable field-shape names:
`__anon` plus a deterministic decimal encoding of the sorted field-name set.
Equal anonymous shapes across modules still share an identical compiler-private
type; different shapes no longer collide after linking.

Covered by `anonymous_record_shapes_do_not_collide_across_modules`, which runs
the same multi-module program on the interpreter and compiled WASM backend.
