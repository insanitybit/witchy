# Sum types

A sum type lists variants. A value is exactly one of them.

```witchy
type Shape:
    Circle(U64)
    Rectangle(Size)
    Empty

type Option<T>:
    Some(T)
    None

fn area(shape: Shape) -> U64:
    match shape:
        Circle(radius):
            return 3 * radius * radius
        Rectangle(size):
            return size.width * size.height
        Empty:
            return 0
```

`tests/fixtures/sum/shapes.wy`, `owned.wy`, `stored.wy` and `tree.wy` run in Wasmtime.

## Rules

- A variant holds nothing, one value, or several (`Full(K, V)`), which it keeps as a record
  whose fields are positions.
- `if let Some(v) = lookup(1):` runs its block for that one variant and `else:` (optional) for
  any other; `else if let` chains (`sum/if-let.wy`). It is a `match` of that arm and a `_` arm,
  so when another variant holds an owner it is refused as a `_` arm that would lose it would
  be: write the whole `match` (`sum/if-let-owner.refused.wy`). `while let Ok(item) =
  items.take_last_mut():` runs while the value matches, evaluating it again on each pass; it is
  `while true:` around such a `match` whose `_` arm leaves the loop (`sum/while-let.wy`).
  On one line `if let` is a value, as `if c: a else: b` is: `let v = if let Ok(n) =
  m.get(&k): *n else: 0`, taken out before its statement as a `match` choosing the value
  (`sum/if-let-value*.wy`). Either one-line form may also stand as a statement or a function's
  last line (`conditional/statement.wy`).
- An arm may have a guard: `Circle(r) if r > 10:` is taken only when the guard holds, and
  otherwise the arms after it are tried in order (`sum/guards*.wy`). A later arm for that
  variant, or `_`, is then required. The value is matched again when a guard fails, so it is
  a name or a field of one and holds no owner (refused as a use after a move); a guard follows
  a pattern one level deep, at most 8 to a match. A guard reads what its arm binds and does
  not name the matched value, so it cannot change what the later arms see
  (`guards-mutate.refused.wy`). It is rewritten to an `if` in the arm whose
  `else` is a `match` of the later arms, so it adds no rule of its own.
- An arm, or any block, that does nothing is written `()`: `Ok: ()` after a push whose
  success needs nothing more (`tests/fixtures/sum/empty-arm.wy`).
- A variant name belongs to one declared sum, so `Circle(2)` needs no type prefix.
- A variant of a generic sum takes its type from the context: a return type, a parameter
  or an annotated `let`. `let a = Some(1)` is refused.
- A `match` has one arm for every variant. A missing arm is refused by name, so adding a
  variant turns every match that must change into an error.
- `_:` written last is the arm of every variant that has none of its own, for a match that
  asks to be open to new variants. `Variant(_):` leaves that variant's value unnamed. Both
  are refused where they would lose an owner (`the `_` arm would lose the owner that `Full`
  holds`); through a reference nothing is taken, so there they are always allowed. A `_`
  arm that no variant reaches is refused. Fixture: `tests/fixtures/match/rest.wy`.
- An owner inside a variant is still owned: it is moved in, moved out by the match, and must
  be consumed in that arm.
- A sum can hold another sum (`Result<Option<U64>, Shape>`).
- A sum can be a record field and the element of a `Buffer`, so `Vec<Option<T>>` and
  `Vec<Shape>` work with the library as it is. A sum with a variant that holds a reference
  cannot be stored, including when the reference is inside another sum: it lives
  in locals only. Its loan follows nested constructors, calls, bindings and matches.
- A sum or a record can hold itself, but only inside a `Buffer` (directly, or through a type
  such as `Vec` whose field is the `Buffer`): the handle has a fixed size, the value would
  not. Such a type is an owner. This is how a tree is written:

  ```witchy
  type Json:
      Null
      Number(U64)
      Array(Vec<Json>)
  ```
- `Result<A, B>` is the built-in sum with variants `Ok` and `Err`. `?` works on `Result` only.

## Matching through a reference

```witchy
fn total(value: &Json) -> U64:
    match value:
        Null:
            return 0
        Number(n):
            return *n
        Array(items):
            var sum: U64 = 0
            for item in items:
                sum += total(item)
            return sum
```

When the matched value is a shared reference to a sum, the sum stays where it is and each
binder is a shared reference to its variant's value: `n` is `&U64`, `items` is `&Vec<Json>`.
The binders keep whatever the matched reference borrowed lent, by the ordinary rule
(`docs/wasm-references.md`), so an owner inside a variant can be read but not moved out.
`match &record.field:` works the same way. `tests/fixtures/references/match.wy` runs in
Wasmtime. A function with a memory or record proof cannot do this yet; one whose contract is
about numbers can (`docs/wasm-scalar-proofs.md`).

## Patterns inside patterns

```witchy
match find(key):
    Ok(Some(value)):
        return value
    Ok(None):
        return 1000
    Err(.Missing):
        return 2000
    Err(.Locked(by)):
        return 3000 + by
```

Inside an arm's parentheses a variant may stand where a name does, to any depth. A variant
is told from a name by its dot, its own parentheses or its capital letter. The arms of one
outer variant become one arm whose body matches that variant's value against the inner
patterns (`src/compiler/nested_patterns.rs`), so each level is checked as an ordinary match:
every inner variant needs an arm, an owner is moved in level by level and still has to be
consumed, and through a reference every level is read in place.

`Ok(_)` after the nested `Ok(...)` arms takes the cases they leave, and the outer `_:` arm
takes whatever no pattern names at any level; neither may leave an owner unnamed.
`Ok(other)` beside nested `Ok(...)` arms is refused: the value has already been taken
apart. `tests/fixtures/match/nested.wy` runs in Wasmtime.

## Representation

A sum value is a tag followed by each variant's own slots side by side, held in Wasm locals.
Stored in memory (a Buffer cell, a frame, behind a reference) it is a 4-byte tag, then the
active variant's value laid out as its own type, at that type's alignment after the tag: the
variants share those bytes, so a stored sum is as large as its tag and its largest variant,
not the sum of all of them (`Result<U64, U64>` is 16 bytes, was 24). A reference to a
variant's value is still an ordinary reference to where it is stored.

Every write of a sum to memory writes the active variant's cells only: each carrier slot
has the tag conditions under which it is live (`Layouts::guards`), and the store of a slot is
conditional on them, at every level of nesting. Reads of a slot that is not live give zero.
`sum/shared-bytes.wy` writes variants over one another in the frame, nested, and in a Vec;
removing either the frame or the heap condition makes it fail. A sum with one variant that
holds a value needs no condition.

Measured against the side-by-side layout: modules grow 3-10% on sum-heavy programs
(`json/roundtrip` 35,353 to 36,755 bytes, `sum/tree` 6,077 to 6,261), for the
conditional stores; instructions run change by under 1% either way (`sum/tree`
7,432 to 7,161, `json/roundtrip` 381,010 to 381,650); programs without stored
multi-variant sums are unchanged. What shrinks is memory: a Buffer of
`Result<U64, U64>` takes 16 bytes a cell instead of 24.
A match compares the tag with each variant in turn and traps on any other tag. The same
code path builds and matches `Result`.

## Not yet

| Gap | Effect |
|---|---|
| Contracts name `Ok` and `Err` only | `ensures match result:` is refused on a declared sum |
| Memory and record proofs are chosen by the shape of the function's signature | a contract about a declared sum's contents cannot be stated; a contract about numbers can sit on any non-generic function |
| No nested pattern in `let ... else` | written as a `match` |
| No sealed sums | written with a declared sum and a record; anonymous sums are in `docs/wasm-unions.md` |

## Variants that hold several values

```
type Shape:
    Dot
    Circle(U64)
    Box(U64, U64)

let shape = Box(3, 4)
match &shape:
    Box(width, height):        // through a reference: each name is a reference
        return *width * *height
```

`Box(U64, U64)` holds the anonymous record `.{0: U64, 1: U64}`. `Box(3, 4)`
makes that record; `Box(width, height):` binds it and opens it, by value with
`let .{0: width, 1: height} = move held` and through a reference with one
`&(*held).N` per name. Nothing below the parser and the match rewrite knows
about it: layout, ownership, storage in a Vec and generics are those of
anonymous records. The same forms work in anonymous sums
(`.[Empty | Between(U64, U64)]`, `.Between(low, high)`) and inside nested
patterns (`Ok(Box(width, height))`).

No source can write a label that is a number, so such a value is made and
opened only by position.

Refused: a pattern with fewer or more names than values
(`tests/fixtures/variants/too-few.refused.wy`); `_` for a value that owns
something, which would drop it silently
(`tests/fixtures/variants/dropped-owner.refused.wy`).

Not done: `let Box(width, height) = shape else:`; a nested pattern in one
position (`Pair(Some(x), y)`); `Ok(a, b)`, since `Result`'s payload type would
have to be written.

## Several patterns for one arm

```
match shape:
    Circle(side) | Square(side):
        return side
    Dot | Box(_, _):
        return 0
```

`A | B:` is an arm for each pattern with the one body, written once by the parser.
The body is checked once for each pattern, so a name it uses must be bound by every
pattern, and with a type the body accepts each time; nothing else is new. Nested
patterns, anonymous variants, matches through a reference and matches used as
values take alternatives the same way. The cost is the body's code once per
pattern.

`Variant(_, _)` matched by value used to be refused as a duplicate local: each `_`
was a local of that name. A field under `_` is now a local no source can name. An
owner under `_` is still refused, as any owner left in scope is
(`tests/fixtures/variants/dropped-owner.refused.wy`).

`tests/fixtures/match/alternatives.wy` runs in Wasmtime. Three `*.refused.wy` beside
it must not compile: a name one pattern does not bind, a name bound as a Bool by one
pattern and a number by the other, and alternatives that leave a variant out.

## Variant names

Two types may each have a variant of one name: `FsError` and `NetError` both
have `Io`, `TooLarge`, `NotText` and `OutOfMemory`, so `import fs` and
`import net` go together. Where a variant is built, the type expected there (a
return type, a parameter, an annotated binding) says which; with nothing to say
and more than one candidate, it is refused, naming the types; naming the type
settles it anywhere: `Right.Io(5)`, `Left.Other`
(`sum/shared-variant.wy`, `sum/shared-variant-ambiguous.refused.wy`). A match
reads its arms against the matched value's own type, so arms need nothing more.
Within one type a variant name is used once.
