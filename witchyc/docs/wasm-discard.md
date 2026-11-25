# Discarding a value and what it owns

`library/wasm-discard.wy` is plain Witchy code on the public functions of
`Vec`. It adds no compiler rule.

```
trait Discard:
    fn discard(self: Self)
```

`value.discard()` consumes the value and releases every allocation it owns.
It is implemented for `U64` and `Bool` (which own nothing), `String`,
`Vec<T>` and `HashMap<K, V>` whose parts are `Discard`, and for `Option<T>` and
`Result<T, E>` whose values are (so a `Vec<Option<String>>` is discarded in one call;
`tests/fixtures/discard/option.wy`). So `Vec<String>`,
`Vec<Vec<String>>` and `HashMap<String, Vec<String>>` are each discarded in
one call. Each impl sits in the library of its own type. The trait itself, with the impls for the
numbers and for `Result`, is `library/wasm-discard-core.wy`, which imports nothing: `option`
imports that file alone, so a program that uses `Option` and no `Vec` is given no `Vec`
(`import discard` gives `Vec`'s impl; since 2026-11-22 a file sees only what it imports itself,
so one that names the trait, `impl Discard for T:` or a `T: Discard` bound, imports
`discard_core` too: docs/wasm-modules.md, "What a file sees").
A type with owners of its own implements the trait by taking itself apart
and discarding each part; `Json` and `Member` in `library/wasm-json-tree.wy`
do, and each calls the other through `Vec<Json>` and `Vec<Member>`.

`Vec.close` is still the way to close a Vec of Copy values. It clears cells
without looking at them, which is only right when a cell owns nothing.

The method is `discard` and not `close` because a method name belongs to its
type: `String.close` exists and carries a proof, and a trait method cannot
carry a contract yet.

Fixture: `tests/fixtures/discard/words.wy`.

## Checked and trusted

Nothing new is proved. The ownership checker refuses a `discard` that leaves
an owner behind, since every path must consume what it holds; that checker is
trusted. That every allocation really goes back is tested (the fixture
allocates again afterwards), not proved.

## Not yet

Nothing is discarded for you:
an owner that goes out of scope is still an error, not an implicit call.
