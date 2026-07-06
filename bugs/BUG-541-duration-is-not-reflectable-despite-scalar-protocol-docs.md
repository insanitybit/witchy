# BUG-541: `Duration` is not reflectable despite scalar protocol docs

Severity: MED
Status: FIXED
Component: `Duration`, `Reflect`, JSON reflection, `derive(Reflect)`, scalar protocol consistency
Fixed: 2026-07-06 (`d88afae`)

## Resolution

Fixed by `d88afae` (`std: fill reflect protocol gaps`).

`std/reflect.witchy` now implements `Reflect for Duration`, reflecting a
duration as its whole millisecond count. `json.stringify(1500ms)` emits `1500`,
and `reflect.debug(duration.seconds(2))` emits `2000`.

Validation: `./scripts/check.sh` green in
`/Users/cobrien/workspace/witchy-reflect-protocol`:
`1446 passed / 2 skipped`, plus build, clippy, Witchy fmt, and wasm playground
build.

## Summary

`Duration` is a built-in scalar type and has a first-class `Show` impl, but it
does not implement `Reflect`. That means the reflection-backed surfaces reject
ordinary duration values:

- `reflect.debug(90s)` fails.
- `json.stringify(90s)` fails.
- `derive(Reflect)` fails for a record with a `Duration` field.

This contradicts the public reflection story that built-in scalars are
reflectable out of the box, and it makes `Duration` an exception among the
simple values users are likely to put in data records.

## Reproduction

```witchy
// scratch/repro-duration-reflect-debug.witchy
import reflect

fn main(console: Console):
    print(console, reflect.debug(90s))
```

Current result:

```text
type error: `Duration` does not implement `Reflect` (no `impl Reflect for Duration`) — required by a call to `reflect`
```

```witchy
// scratch/repro-duration-json-stringify.witchy
import json

fn main(console: Console):
    print(console, json.stringify(90s))
```

Current result:

```text
type error: `Duration` does not implement `Reflect` (no `impl Reflect for Duration`) — required by a call to `reflect`
```

```witchy
// scratch/repro-duration-derived-reflect.witchy
import json
import reflect

type Job derive(Reflect):
    timeout: Duration

fn main(console: Console):
    print(console, json.stringify(Job(90s)))
```

Current result:

```text
type error: `Duration` does not implement `Reflect` (no `impl Reflect for Duration`) — required by a call to `reflect`
```

Control:

```witchy
// scratch/repro-duration-show-control.witchy
import show

fn main(console: Console):
    show.say(console, 90s)
```

Current result:

```text
1m30s
```

## Evidence

- `spec/language.md:79-80` lists `Duration` with the built-in types.
- `std/duration.witchy:1-7` describes `Duration` as a built-in type carried as
  whole milliseconds.
- `std/show.witchy:1-4` says built-in `Show` impls cover scalar types including
  `Duration`, and `std/show.witchy:43-45` implements it via `duration.human`.
- `std/reflect.witchy:1-13` says scalars are reflectable out of the box and the
  scalar impls below are the leaves.
- `std/reflect.witchy:20-29` defines `Mirror` leaves for `Int`, `Float`,
  `Bool`, `String`, lists, tuples, records, variants, and nil, but has no
  duration leaf.
- `std/reflect.witchy:43-57` implements `Reflect` for `Int`, `Float`, `Bool`,
  and `String`, but not `Duration`.
- `std/json.witchy:757-764` routes `json.from_value` / `stringify` through
  `Reflect`, so the missing duration mirror also blocks JSON serialization.
- `std/meta.witchy:155-190` makes `derive(Reflect)` call `reflect_one(...)` for
  ordinary fields; a `Duration` field therefore requires `Duration: Reflect`.

## Why This Feels Bad

The idea is good: `Duration` is not just an alias for `Int`; it is a distinct,
domain-specific scalar with arithmetic, parsing, and human display. That is the
right level of abstraction.

The bad part is that the core protocols do not agree on whether it is ordinary
data. `Show` knows how to present it. Interpolation is being moved toward
honoring that `Show` surface. But `Reflect` and JSON-through-reflection reject
it entirely, even though the docs use broad scalar language and `Duration` is
one of the built-ins a user would naturally put in config, task, timing, and
server records.

For a public release, this makes the language feel more special-cased than it
needs to be: users learn a clean scalar type, then hit a protocol cliff when
they try to debug or serialize it.

## Desired Direction

Pick one explicit reflected representation for `Duration` and make it consistent
across `reflect.debug`, `json.stringify`, and `derive(Reflect)`:

- raw milliseconds as `MInt(duration.to_milliseconds(d))` / JSON number;
- human duration text as `MString(duration.human(d))` / JSON string;
- or a dedicated mirror leaf such as `MDuration(Duration)` with documented JSON
  lowering.

The representation choice is less important than making the boundary explicit.
If `Duration` should not be JSON-reflectable, then reflection docs should stop
claiming scalar coverage and the diagnostic should name the deliberate policy.

Acceptance checks:

- `reflect.debug(90s)` has documented output.
- `json.stringify(90s)` has documented output, or a focused deliberate rejection
  distinct from a missing trait impl.
- `type Job derive(Reflect): timeout: Duration` checks and `json.stringify`
  works according to the chosen representation, or the unsupported policy is
  documented with a direct error.
