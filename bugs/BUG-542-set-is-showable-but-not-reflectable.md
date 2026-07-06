# BUG-542: `Set` is showable but not reflectable

Severity: MED
Status: FIXED
Component: `Set`, `Reflect`, JSON reflection, `derive(Reflect)`, collection protocol consistency
Fixed: 2026-07-06 (`d88afae`)

## Resolution

Fixed by `d88afae` (`std: fill reflect protocol gaps`).

`std/reflect.witchy` now implements `Reflect for Set(a) where a: Reflect`,
reflecting a set as the insertion-order array view returned by `set.to_list`.
This intentionally supports encoding/debugging, not generic reconstruction.

Validation: `./scripts/check.sh` green in
`/Users/cobrien/workspace/witchy-reflect-protocol`:
`1446 passed / 2 skipped`, plus build, clippy, Witchy fmt, and wasm playground
build.

## Summary

`Set(a)` has a polished collection surface: it is sealed, compares by
membership, iterates, and has a blanket `Show` impl. But it does not implement
`Reflect`, so the reflection-backed surfaces reject ordinary set values:

- `reflect.debug(s)` fails for `Set(Int)`.
- `json.stringify(s)` fails for `Set(Int)`.
- `derive(Reflect)` fails for records with `Set(...)` fields.

This is not a broken set operation; it is a protocol completeness gap. Users can
display a set, but cannot debug or serialize the same value through the standard
reflection path.

## Reproduction

```witchy
// scratch/repro-set-reflect-debug.witchy
import reflect
import set

fn main(console: Console):
    let s: Set(Int) = set.from_list([1, 2])
    print(console, reflect.debug(s))
```

Current result:

```text
type error: `Set<Int>` does not implement `Reflect` (no `impl Reflect for Set<Int>`) — required by a call to `reflect`
```

```witchy
// scratch/repro-set-json-stringify.witchy
import json
import set

fn main(console: Console):
    let s: Set(Int) = set.from_list([1, 2])
    print(console, json.stringify(s))
```

Current result:

```text
type error: `Set<Int>` does not implement `Reflect` (no `impl Reflect for Set<Int>`) — required by a call to `reflect`
```

```witchy
// scratch/repro-set-derived-reflect.witchy
import json
import reflect
import set

type Bag derive(Reflect):
    values: Set(Int)

fn main(console: Console):
    let s: Set(Int) = set.from_list([1, 2])
    print(console, json.stringify(Bag(s)))
```

Current result:

```text
type error: `Set<Int>` does not implement `Reflect` (no `impl Reflect for Set<Int>`) — required by a call to `reflect`
```

Control:

```witchy
// scratch/repro-set-show-control.witchy
import set
import show

fn main(console: Console):
    let s: Set(Int) = set.from_list([1, 2])
    show.say(console, s)
```

Current result:

```text
{1, 2}
```

## Evidence

- `std/set.witchy:1-7` presents `Set(a)` as a normal collection and says a
  `Set` whose members are `Show` renders through `show` / `say`.
- `std/set.witchy:13-25` seals the representation and implements membership
  equality through `PartialEq` / `Eq`.
- `std/show.witchy:1-7` lists `Set` with the built-in container `Show` surface.
- `std/show.witchy:116-121` implements `Show for Set(a) where a: Show`.
- `std/reflect.witchy:1-9` frames reflection as the generic structure path for
  debug and JSON, but lists only `List`, `Option`, tuples, and `Dict` among
  built-in containers.
- `std/reflect.witchy:95-130` implements `Reflect` for `List`, `Option`, and
  `Dict`; `std/reflect.witchy:135-148` handles tuples through arity four. There
  is no `Reflect for Set(a)`.
- `std/json.witchy:757-764` routes `json.from_value` / `stringify` through
  `Reflect`, so missing set reflection also blocks JSON serialization.
- `std/meta.witchy:155-190` makes `derive(Reflect)` call `reflect_one(...)` for
  ordinary fields; a `Set(...)` field therefore requires `Set(...): Reflect`.

## Why This Feels Bad

The good design is that `Set` has become a real collection, not a loose list
alias: it is sealed, normalizes duplicates through constructors, has set
algebra, and renders like a collection.

The bad part is that protocol coverage stops halfway. `Show` knows `Set` is a
container, but `Reflect` does not. That means a user can print a set in a
console, but cannot include one in a data record they want to inspect with
`reflect.debug` or encode with `json.stringify`.

For a public release, this reads as an implementation boundary leaking into the
language model. If `Set` is a normal stdlib collection, the core data protocols
should either include it or document why set semantics are intentionally not a
JSON/reflection shape.

## Desired Direction

Pick a deliberate reflected representation for sets:

- reflect as `MList` / JSON array using `set.to_list(s)` insertion order;
- add a distinct mirror leaf such as `MSet(List(Mirror))` and lower it to JSON
  arrays deliberately;
- or document that sets are displayable but not reflectable/JSON-serializable,
  with a focused diagnostic instead of a missing-trait failure.

Acceptance checks:

- `reflect.debug(set.from_list([1, 2]))` has documented behavior, or a focused
  deliberate rejection.
- `json.stringify(set.from_list([1, 2]))` has documented behavior.
- `type Bag derive(Reflect): values: Set(Int)` checks and JSON stringification
  follows the chosen representation, or the unsupported policy is explicit.
