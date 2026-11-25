# derive

```
type Point derive(Eq, Hash, Before, Discard):
    x: U64
    y: U64
```

`derive` writes the impls a program would write by hand, as source, which is then parsed
like any other: the checker and the proofs see ordinary methods.

| Trait | Made as |
|---|---|
| `Eq` | every field equal: `(*self).x == (*other).x && ...` (`==` on a value that is not a number is `eq`) |
| `Hash` | each field's `hash()` folded in by `hash_fold` (library/wasm-hash.wy), a rotation and an exclusive or that cannot overflow |
| `Before` | in field order, the first field that differs decides; numbers by `<`, other fields by their `before` |
| `Discard` | the record opened and each field discarded through its own impl, so owners are closed |

A field whose type lacks the trait is refused as a call to a missing method (`F64` has no
`Hash`, since a NaN is not equal to itself: `derive/no-hash.refused.wy`). `U8`, `U16`, `U32`
and `Bool` hash as the U64 they widen to (`derive/narrow-key.wy`). The traits come from their libraries, as for hand-written
impls: `import hash` for `Eq` and `Hash`, `import sort` for `Before`; `Discard` comes with
the containers (`vec`, `hashmap`). A derived `Point` is a
`HashMap` key and sorts in a `Vec` (`derive/point.wy`); a record holding a `String` derives
`Eq`, `Hash` and `Discard` (`derive/owner.wy`).

`Before` exists for every number type and for `Bool` (`false` first), `String` and `Duration`, so a
record of them derives it (`derive/before-small.wy`).

## Sum types and generic types

A sum type derives the same four. `Eq`: the same variant holding equal values. `Hash`: the
variant's position folded with each value's hash. `Before`: variants in the order written, then
the values in order. `Discard`: each value discarded. A variant may hold nothing, one value or
several (`Line(U64, U64)`) (`derive/sum.wy`, `derive/sum-several.wy`).

A generic record or sum type derives with each type parameter bound by the trait:
`type Pair<T> derive(Eq)` is `impl<T: Any + Eq> Eq for Pair<T>`, so `Pair<Plain>` has `Eq` only
when `Plain` does, and is refused at the use otherwise (`derive/generic*`); `Any` lets `T` be an
owner, so `Pair<String>` derives too (`derive/generic-owner.wy`).

`Name.method()` calls a type's method, so a variant holding nothing is bound before a method is
called on it: `let empty = Nothing` then `empty.discard()` (`derive/sum-variant-call.refused.wy`).

## Recipes from a module

A module can declare its own derive, a recipe (docs/plan-derive-laws.md, slice 1):

```
pub derive Key:
    impl Eq for Self:
        fn eq(self: &Self, other: &Self) -> Bool:
            static for f in fields:
                if !((*self).f == (*other).f):
                    return false
            return true
```

Apply the recipe with `type Quad derive(keys.Key):`, or select fields with
`type Account derive(keys.Key(by: id)):`. `Self` stands for the deriving type.
A recipe can also use `For`, supplied at the derive site by
`derive(keys.Query(for: NameKey))`. When both arguments are present, `for:`
comes before `by:`. A recipe without `pub` is private to its own module.

A recipe expands to `impl` blocks for the deriving type. It can repeat over
selected record fields with `static for f in fields:` and over sum variants
with `static for v, tag in variants:`. Inside a field loop,
`static if f is U32:` and `static else:` select a branch from the field's
declared type. In a variant arm, `v(..mine)` binds
that variant's payloads. `static for a, b in mine, theirs:` pairs the payloads
bound by two patterns. The sum form uses variant order from the declaration.
`by:` selects record fields. [Record fixture](../tests/fixtures/derive/recipe-two-types.wy) ·
[Sum fixture](../tests/fixtures/derive/recipe-variants.wy)

Names written in the recipe resolve in the recipe's module, including its
private helpers and imported names. Names substituted at the derive site,
including the target type, the `for:` type, and field names, retain the context
of that site. The generated methods can construct the deriving type and use
its sealed fields. Access to that type is scoped to it. Field types and the
`for:` type retain their own privacy rules.
[Definition-site fixture](../tests/fixtures/derive/recipe-module-helper.wy) ·
[Recipe on a sealed target](../tests/fixtures/derive/recipe-sealed-target.wy) ·
[Refused sealed access](../tests/fixtures/derive/recipe-for-sealed-write.refused.wy)

## The key law

A type that implements `HashKey` (a `KeyMap`
key or query; `KeyMap` is `import keymap`) is admitted only when its bodies
have the shape that makes the map correct, checked on every build (`src/compiler/key_law.rs`):
`eq` tests some fields (`(*self).f == (*other).f`, joined by `&&` or as early `return false`
steps), `feed` feeds some of those fields once each, and every field's type is a library root
(U8, U16, U32, U64, Bool, String, Span, Text), a record whose own impls pass, or a type
parameter. A query type (`HashKey` and `Equivalent<K>`, no `Eq`) pairs its fields with the
key's in `equivalent`, and its feed, through those pairs, is the key's feed in order. Refused:
feeding a field `eq` does not test, feeding one twice, any other statement in either body, F64
fields, fields of a type with no checked law, a query that feeds more than its key
(`policy/law-*.refused.wy`). The law follows from the shape for every type at once, so it costs
no proof per type or instantiation. `Hash` and `HashMap` are unchanged and unchecked.

## Target selection in ordinary code

Use `static if` to choose a branch before type checking. For example:

```witchy
fn target_choice() -> U64:
    static if target.backend == .Wasm:
        return 1
    else:
        return 2
```

The target facts are `target.backend` (compare with `.Wasm` or `.Native`)
and `target.os` (compare with `.MacOS` or `.Linux`). Wasm matches neither
OS. Facts describe the requested build target. The optional fallback can
also be written `static else:`, the spelling used in derive recipes.

The compiler retains the selected block at its enclosing indentation before
name resolution, type checking, authority reporting, and proof generation.
Both arms must be lexically valid. An absent fallback emits nothing. Keep
imports unconditional. This form does not yet query CPU extensions or run
arbitrary compile-time expressions. Recipe field tests still use the recipe
form `static if field is Type:`.

Direct builds accept `--profile=debug` and `--profile=release`. Release is
the default. Use `static if target.profile == .Debug:` to select diagnostics
before source checking. This profile controls source selection.
