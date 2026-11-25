# Traits

```
trait Area:
    fn area(self: &Self) -> U64

impl Area for Square:
    fn area(self: &Square) -> U64:
        return (*self).side * (*self).side

fn twice<T: Area>(shape: &T) -> U64:
    return shape.area() + shape.area()
```

A `trait` lists method signatures. `Self` is the implementing type.
`impl Trait for Type` provides them; its methods are ordinary methods of the
type, so `square.area()` works with or without the trait in sight.

A type parameter is bound with `T: Trait`, or several with `T: Any + Area +
Same`. Inside the function a value of type `T` has the methods of its bounds
and nothing else. A call must give each bounded parameter a type that
implements the trait, or one of the caller's own parameters with the same
bound.

Scalars can implement traits (`impl Same for U64`). A generic type implements
a trait for every instance, with bounds of its own if it needs them:

```
impl<T: Same> Same for Pair<T>:
    fn same(self: &Pair<T>, other: &Pair<T>) -> Bool:
        ...(*self).first.same(&(*other).first)...
```

`Pair<Square>` then implements `Same`, and `Pair<Plain>` does not unless
`Plain` does. `tests/fixtures/traits/shapes.wy` runs all of this in Wasmtime,
including `Pair<Pair<U64>>`.

## How

There is no dictionary and no dynamic dispatch. A trait method is a function
`Trait.method<Self>` with no body. Generic functions are checked once against
it, like any generic call. When the compiler makes the concrete instance of a
generic function, `Self` is known, and the call becomes a call of
`Type.method`. The emitted Wasm has only direct calls.

`src/compiler/methods.rs` checks an impl against its trait (same parameters and
result with `Self` replaced, every method present, none extra).
`check.rs` checks bounds at calls. `instances.rs` replaces the calls.

## Refusals

- "`Plain` does not implement `Area`": a call or a conditional impl needs it.
- "no trait this type parameter is bound by has a method of this name".
- "this method's parameters and result must match the trait's, with `Self` as
  the implementing type".
- "an impl of a trait provides every method of the trait"; "this method is
  not one of the trait's"; "a type implements a trait once".
- "an implementation of a trait method cannot add `requires`": a generic
  caller sees only the trait's signature and could not be asked to establish
  more.

## Not yet

A method name belongs to its type, so two traits with a method of the same
name cannot both be implemented by one type. No default methods, no
associated types or functions without a receiver (`T.zero()`), no trait
objects. `==` and `!=` on values that are not numbers or Bools go through `Eq.eq`; no
other operator goes through a trait. A trait
method's contract speaks of its numbers, its result and pure methods of `self` ("Contracts on
trait methods" below).

## `Eq` and `==` (2026-11-15)

The library's equality trait is `Eq` (`library/wasm-hash.wy`), which was called
`Same`:

```
trait Eq:
    fn eq(self: &Self, other: &Self) -> Bool
```

`a == b` on values that are not numbers or Bools is `a.eq(&b)`, and `a != b`
is `!a.eq(&b)`; an operand that is already a reference is passed as it is
(`left == right` with two `&String`). Numbers, Bools and I64 compare as before.
A type without `Eq` is refused with a message that names it
(`tests/fixtures/eq/`). `String`, `U64` and `I64` implement it, and HashMap keys
need `Hash + Eq`.

Not yet: anything that checks that `eq` is an equivalence, or that equal keys
hash the same. Witchy 1 keeps those laws in separate markers (`Eq` beside
`PartialEq`, `LawfulEq`, rfcs/0175); here they would be proved properties of an
implementation. 
## Traits with type parameters: `Equivalent<K>` (2026-11-15)

```
trait Equivalent<K>:
    fn equivalent(self: &Self, key: &K) -> Bool

impl Equivalent<String> for Text:
    fn equivalent(self: &Text, key: &String) -> Bool:
        ...

fn get<Q: Any + Hash + Equivalent<K>>(map: &HashMap<K, V>, key: &Q) -> Result<&V, U64>:
```

A trait may take type parameters. An `impl` gives an argument for each
(`impl Equivalent<String> for Text`), and its methods must match the trait's
signatures with `Self` and the parameters put in. A bound names the arguments
(`Q: Equivalent<K>`).

The arguments are checked exactly. Where the function is called, each bound's
arguments are worked out for that call and must be the ones the type
implements: a parameter of the caller by one of its own bounds with the same
arguments, a concrete type by its `impl`. So a call through `Q: Equivalent<K>`
compares only with a `K` (`traits/generic-other-type.refused.wy`, which uses a
trait of its own, `Comparable<K>`), and a type whose `impl` names another
argument does not qualify (`generic-not-implemented.refused.wy`).

With keys now taken by any `Q`, `map.get(&7)` would leave `Q` open; a bare
number left open by everything else is a `U64`, as it is wherever no other type
is asked for.

This is hashbrown's `Equivalent` rather than Rust's `Borrow`: a lookup takes any
type that can compare itself with the key type and hashes as it does. `HashMap`
`get`, `get_mut`, `contains` and `remove` take a `Q: Any + Hash + Equivalent<K>`,
so a `HashMap<String, V>` is searched with a `Text` literal and no String is
made (`tests/fixtures/hashmap/text-key.wy`). `U64`, `I64` and `String` are
`Equivalent` to themselves, and `Text` to `String`; `Text` hashes as the String
of the same bytes. A program's own key type needs `impl Equivalent<Key> for Key`
beside its `Eq`, since there are no blanket impls.

Limits: a type implements a given trait once, whatever the arguments; an impl's own bounds
take no arguments. An impl of a trait with parameters may have type parameters of its own
(`impl<T> Walk<T> for Vec<T>`, since 2026-11-24), but a bound that gives the trait arguments
(`C: Walk<U64>`) is not met by such an impl yet: its arguments name its parameters, and the
checker does not read them per instance (`traits.rs` `satisfies` says so). Nothing checks that an `Equivalent` hashes as the key, or that
`eq` is an equivalence.

Trusted, as the rest of the trait rules: the checker's bound check, which is
tested on both sides, not proved. The bound's text is read back as a type
(`parser::parse_type`) where it is checked.

## Impls for every type that meets a bound

```
impl<K: Any + Eq> Equivalent<K> for K:
    fn equivalent(self: &K, key: &K) -> Bool:
        return self.eq(key)
```

An impl whose type is its own one parameter gives the trait to every type that
meets the parameter's bounds. The library uses it so that any `Eq` key is
equivalent to keys of its own type, as hashbrown does; a program's key type now
needs only `Hash` and `Eq`.

- A trait has at most one such impl.
- A type that would have the trait from it cannot also implement it directly
  with the same arguments: one impl per type and trait.
- Its methods are generic functions under a name no source can spell. A call on
  a value of a concrete type, or through a bound naming the trait, reaches them
  when the type has no impl of its own.
- The impl's trait arguments may be its parameter or concrete types.
- A type with a method of the same name as one of the trait's, not from
  implementing the trait directly, is refused: a trait call would reach it.

Fixtures: `tests/fixtures/traits/blanket*.wy`.

## Contracts on trait methods (2026-11-17)

A trait method may carry a contract over its numbers and its result, written as clauses with
no body:

```
trait Score:
    fn score(self: &Self, cap: U64) -> U64:
        requires cap <= 1000
        ensures result <= cap
```

- Every implementation, a blanket one included, takes the clauses as its own and is proved
  against them (`auto`), with any `ensures` of its own added. It adds no `requires`, and
  names its parameters as the trait does.
- Generic code calling the method through a bound shows the `requires` at the call and
  relies on the `ensures` after it, as for any function with a contract: `fn total<T:
  Score>(a: &T, b: &T) -> U64: ensures result <= 2000` is proved from the bound alone.
- A clause may also read a `pure fn` of the same trait on `self` (or on a parameter of a
  bounded type): `pure fn len(self: &Self) -> U64` and `fn get(self: &Self, index: U64) -> U64:
  requires index < self.len()`. Every implementation of a pure trait method is pure
  (`law-impure.refused.wy`), so two calls on an unchanged value agree: generic code that asked
  `items.len()` may rely on it until the value may have changed (a call given `&mut items`
  forgets it, `law-len-changed.refused.wy`). An implementation proves the clause with its own
  `len` (`law-len.wy`; `law-len-unshown.refused.wy`). Clauses about `self`'s fields directly
  are still not stated.

Fixtures: `tests/fixtures/traits/contract*.wy`.

## `impl Trait` parameters

`fn least(a: impl Before, b: impl Hash) -> U64` is `fn least<A: Before, B: Hash>(a: A, b: B)`
with type parameters no source can name, one for each such parameter, so two `impl Before`
parameters may be different types (`traits/impl-parameter.wy`). Like a type parameter without
`Any`, it takes copied values, not owners (`impl-owner.refused.wy`); a function that takes an
owner writes `<T: Any + Trait>`. It is written only as a parameter's type
(`impl-elsewhere.refused.wy`); `-> impl Trait`, a result whose type is hidden, is not taken.

## Operators

On a value that is not a number, `a + b` is `Add.add(a, b)`, `a - b` is `Sub.sub(a, b)` and
`a * b` is `Mul.mul(a, b)`, from the traits (`import ops`, library/wasm-ops.wy, both operands of
the one type, copied in): the trait's method, never another method of the same name
(`operators-own-method.refused.wy`). `a < b` is `Before.before(&a, &b)`, `a > b` is
`sort_after(&a, &b)` (whether `b` is before `a`, with `a` evaluated first), and `<=`/`>=`
their negations (`operators-order.wy`), from `Before` (which `derive(Before)` makes); `==` is `eq`
from `Eq`. `value * n`, with `n` a U64, is `Scale.scale(value, n)` (`250ms * 4`); any other
number beside a value of the type is refused. `total += price` on a record is
`total = total + price`. Numbers keep their own
operators. A type without the trait is refused with the trait's name
(`traits/operators.wy`, `operators-missing.refused.wy`). A chain (`a + b + c + ...`) is typed
in steps in proportion to its length; one expression holds up to 41 such operators, and a
longer one is refused at once (`generic inference complexity limit`: give its parts names with
`let`; `traits/operator-chain.wy`, `operator-chain-depth.refused.wy`).
