# BUG-543: `Result` is showable but not reflectable

Severity: MED
Status: FIXED
Component: `Result`, `Reflect`, JSON reflection, `derive(Reflect)`, error/data protocol consistency
Fixed: 2026-07-06 (`d88afae`)

## Resolution

Fixed by `d88afae` (`std: fill reflect protocol gaps`).

`std/reflect.witchy` now implements
`Reflect for Result(a, e) where a: Reflect, e: Reflect`, reflecting `Ok` and
`Err` as tagged `MVariant` values. Through JSON reflection this encodes as
`{"$variant":"Ok","$values":[...]}` or
`{"$variant":"Err","$values":[...]}`.

Validation: `./scripts/check.sh` green in
`/Users/cobrien/workspace/witchy-reflect-protocol`:
`1446 passed / 2 skipped`, plus build, clippy, Witchy fmt, and wasm playground
build.

## Summary

`Result(a, e)` is one of Witchy's central standard data types: parse APIs,
decoders, and fallible helpers return it, `?` understands it, and `std/show`
has a blanket `Show` impl for it. But `Result` does not implement `Reflect`, so
the reflection-backed surfaces reject ordinary result values:

- `reflect.debug(r)` fails for `Result(Int, String)`.
- `json.stringify(r)` fails for `Result(Int, String)`.
- `derive(Reflect)` fails for records with `Result(...)` fields.

This makes `Result` feel half-integrated: excellent as control-flow and display
data, but not usable in the same debug/JSON path as `Option`, lists, tuples, and
derived user sum types.

## Reproduction

```witchy
// scratch/repro-result-reflect-debug.witchy
import reflect
import result

fn main(console: Console):
    let r: Result(Int, String) = Ok(7)
    print(console, reflect.debug(r))
```

Current result:

```text
type error: `Result<Int,String>` does not implement `Reflect` (no `impl Reflect for Result<Int,String>`) — required by a call to `reflect`
```

```witchy
// scratch/repro-result-json-stringify.witchy
import json
import result

fn main(console: Console):
    let r: Result(Int, String) = Ok(7)
    print(console, json.stringify(r))
```

Current result:

```text
type error: `Result<Int,String>` does not implement `Reflect` (no `impl Reflect for Result<Int,String>`) — required by a call to `reflect`
```

```witchy
// scratch/repro-result-derived-reflect.witchy
import json
import reflect
import result

type Response derive(Reflect):
    parsed: Result(Int, String)

fn main(console: Console):
    let r: Result(Int, String) = Ok(7)
    print(console, json.stringify(Response(r)))
```

Current result:

```text
type error: `Result<Int,String>` does not implement `Reflect` (no `impl Reflect for Result<Int,String>`) — required by a call to `reflect`
```

Control:

```witchy
// scratch/repro-result-show-control.witchy
import result
import show

fn main(console: Console):
    let r: Result(Int, String) = Ok(7)
    show.say(console, r)
```

Current result:

```text
Ok(7)
```

## Evidence

- `std/result.witchy:1-3` presents `Result` as the standard fallible helper type.
- `std/result.witchy:9-11` defines the core `Ok(a)` / `Err(e)` sum type.
- `std/show.witchy:91-101` implements `Show for Result(a, e) where a: Show, e:
  Show`.
- `std/reflect.witchy:66-77` implements `Option` reflection as a standard
  `MVariant`.
- `std/reflect.witchy:95-148` implements built-in container reflection for
  `List`, `Option`, `Dict`, and tuples, but not `Result`.
- `std/json.witchy:757-764` routes `json.from_value` / `stringify` through
  `Reflect`, so the missing `Result` impl also blocks JSON serialization.
- `std/meta.witchy:155-190` makes `derive(Reflect)` call `reflect_one(...)` for
  ordinary fields; a `Result(...)` field therefore requires `Result(...):
  Reflect`.
- `book/src/appendix-stdlib.md:42` describes `json.stringify(x)` /
  `json.from_value(x)` as reflectively encoding values, which makes this
  everyday `Result` exception surprising in public docs.

## Why This Feels Bad

Witchy's `Result` story is otherwise strong: fallible APIs return a typed value,
the `?` operator composes it, and helper functions keep error handling explicit.
That is good public-language design.

The roughness is that `Result` stops being ordinary data at the reflection
boundary. `Option` has a reflective sum representation. User sum types can
derive `Reflect`. But the other canonical prelude sum type, `Result`, cannot be
debugged or encoded through the same mechanism.

This makes data/error APIs harder to showcase. A response object containing a
parsed result, validation outcome, or batch item status should not require a
handwritten conversion just because `Result` is a standard type rather than a
user-declared `derive(Reflect)` enum.

## Desired Direction

Add a deliberate reflected representation for `Result`:

- mirror it as `MVariant("Result", "Ok", [value])` /
  `MVariant("Result", "Err", [error])`, matching `Option`;
- document the JSON lowering of those variants; or
- if `Result` is intentionally control-flow-only for JSON/reflection, document
  that and emit a focused diagnostic rather than a missing-trait failure.

Acceptance checks:

- `reflect.debug(Ok(7): Result(Int, String))` has documented behavior.
- `json.stringify(Ok(7): Result(Int, String))` has documented behavior.
- `type Response derive(Reflect): parsed: Result(Int, String)` checks and JSON
  stringification follows the chosen representation, or the unsupported policy
  is explicit.
