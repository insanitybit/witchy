# Anonymous sums

A declared sum (`docs/wasm-sums.md`) has a name. An anonymous sum is known by
its tags and what they hold, as an anonymous record is known by its labels.
The spelling is Witchy 1's.

```
type ParseError = .[Empty | TooBig(U64)]

fn parse(value: U64) -> Result<U64, ParseError>:
    if value == 0:
        return Err(.Empty)
    if value > 100:
        return Err(.TooBig(value))
    return Ok(value)
```

- **Type.** `.[Tag | Tag(Type)]`. Tag order does not matter:
  `.[TooBig(U64) | Empty]` is the same type. A tag is named once. `type Name =`
  gives the type another spelling, not a new identity.
- **Values.** `.Tag` and `.Tag(value)`. A tag alone has no type; it takes one
  from where it is used: a return, an argument, an annotated binding, the
  payload of `Ok` or `Err`.
- **Match.** Arms are written with the dot: `.Empty:`, `.TooBig(size):`. Every
  tag has an arm, or the match ends with `_:`.
- **Owners.** A tag can hold an owner (`.Taken(String)`). The rules are the
  declared sum's: moved in, moved out by `match move`, consumed in its arm.
- **Storage and generics.** An anonymous sum is stored in a Buffer or a record
  field like a declared one, and can hold a type parameter
  (`.[One(T) | Two(T)]`).

## Widening

A value of one anonymous sum is accepted where a wider one is expected, if
every tag it has is in the wider type and holds exactly the same type. This
happens at arguments, returns, annotated bindings, and through `?`:

```
type LookupError = .[Missing]
type AnyError = .[Empty | Missing | TooBig(U64) | Refused]

fn both(value: U64) -> Result<U64, AnyError>:
    let parsed = parse(value)?      // .[Empty | TooBig(U64)] widens
    let found = lookup(parsed)?     // .[Missing] widens
    return Ok(found)
```

Nothing narrows on its own, and a tag never changes what it holds: both are
refused, with a message that says to match the value and build the other one
by hand. A `Result` is widened by `?` only; `return helper()` where the two
Result types differ in their error set is refused, write `?` and `Ok`.

Widening is not free and not a cast. Tags are numbered in name order within
each type, so the tag is renumbered and each variant's value is moved to its
place in the wider value. An owner inside moves with it and is still owned
exactly once.

## Checked and trusted

Nothing here is proved. A function with a contract cannot build, widen or
match an anonymous sum yet. The checker's tag lookup, the widening test and
the renumbering in the emitter are trusted, with unit tests
(`src/compiler/union_tests.rs`) and two Wasmtime fixtures
(`tests/fixtures/union/errors.wy`, `tests/fixtures/union/owners.wy`).

## Not yet

Widening inside another type, such as `Result<T, Small>` to
`Result<T, Wide>` outside `?`, or `Vec<Small>` to `Vec<Wide>`. Methods and
trait impls on an anonymous sum. Equality.
