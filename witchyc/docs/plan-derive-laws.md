# Plan: library derives with checked key laws

Design: `/Users/cobrien/workspace/generic/witchy-comptime-hashing-20261119/derive-laws.md` and the
handoff in `report.md` there (Astra, 2026-11-19). Started 2026-11-19 on the user's go-ahead.
Syntax is the design's proposal unless a slice records otherwise.

## Where the code is today

- `type T derive(Eq, Hash, Before, Discard)` is expanded **in the parser** (`src/parser/derive.rs`):
  closed list of four names, written out as source and re-parsed. A derive from an imported
  module is not possible: expansion runs per file, before imports resolve.
- `Hash.hash(self) -> U64` per field, folded by `hash_fold` (`library/wasm-hash.wy`); the map
  (`library/wasm-hashmap.wy`) calls `key.hash() % capacity` in find, find_by, get, remove and
  growth. Borrowed lookup is `Equivalent<K>`.
- Legacy oracle to keep: two-U64 `Point(1, 2)` hashes to `335675534306`.

## Slices, in order (each gated, recorded in docs/decisions.md)

1. **Recipe from an imported module.** Move derive expansion after module loading so a derive
   name can resolve to a declaration in an imported library (`derive(keys.Key)`), with the
   built-in four kept as they are. A recipe is a bounded template over the type's descriptor
   (fields in order, their types, the author's selection `by: id`); no general compile-time
   evaluation. Done when two unrelated types in two programs use one recipe from a library and
   the recipe can be edited without changing the compiler.
2. **Policy-parameterized hashing.** A policy hashes the whole logical key (framed bytes, little
   endian, chunk-invariant) with the map's seed; map operations route through it; existing
   `derive(Eq, Hash)` and map outputs unchanged (oracle above).
3. **Checked evidence.** The generated Eq and hash bodies are matched, after type checking and
   before planning, against the recipe's templates (projection, product, leaf witnesses, policy
   entry); a changed body or leaf refuses at the field, adapter or policy site. Measure whether a
   generic product needs a kernel run per instantiation (the stopping rule: if it does, revise).
4. **Wasm comparison and false-law controls** as in the handoff (Quad lanes, Account by id,
   tenant/String key queried by a Span; the six refusals).

Slice 2 design (2026-11-19, revised the same hour). The language has no generic trait methods,
no zero-field records and no default methods, and an impl's own type parameters must be its
target's, so a key trait generic over the policy (`impl<P> HashWith<P> for Quad`) would need
trait resolution changed. Instead, Rust's `Hasher` shape, with no compiler change:

- `Policy`, a sum in `library/wasm-hash.wy` the map holds: `Reference(seed)` (the handoff's
  order-sensitive test policy: `seed % 4294967291`, then `(33 h + byte) % 4294967291` per byte)
  and `Constant` (always 0: consistent, and every key collides). Adding a policy edits this
  library; keys and recipes do not change.
- `HashState`, the policy and its running value: `word(w, count)` feeds the low `count` bytes of
  `w`, little endian; `bytes(&[U8])` a run; `finish()`. Chunk invariance (a run fed as words or as
  bytes gives one state) is each policy's law.
- `trait HashKey: fn feed(self: &Self, state: &mut HashState)`: the whole logical key, framed
  (fixed-width numbers as their bytes; a byte string as its length, eight bytes, then the bytes).
  A recipe writes it with `static for`; borrowed queries implement it the same way.
- A new map routes find, borrowed lookup, removal's backward shift and growth through it;
  `HashMap` and `Hash.hash()` are unchanged (the oracle `Point(1, 2)` = 335675534306 stays).

Slice 3 design (2026-11-19). The law the map needs: `eq(x, y)` implies `x` and `y` feed the
same input, and `Eq` is an equivalence. Both follow from the shape of the two bodies, with no
kernel run per type:

- A program's `impl HashKey for T` is admitted only when it and `T`'s `impl Eq` are the
  template: `eq` tests fields `E` (each `(*self).f == (*other).f`, conjunction or early-return
  form), `feed` feeds fields `F` in some order (each `(*self).f.feed(state)`), `F` is a subset of
  `E`, and every field in `F` and `E` has a lawful leaf. Then equal on `E` implies equal on `F`,
  each leaf feeds equal input for equal values, and the concatenation is equal: the law, proved
  once for the template (here, in prose and in the checker's tests), not per type.
- Lawful leaves: the library's U8, U16, U32, U64, Bool, String, Span and Text impls, listed as
  explicit trust roots (their bodies are the library's); a record type whose own impls pass
  this check; a type parameter `T` only under a bound that carries the law (to settle: a
  `KeyLaw` marker or the map's bound; the diagnostic names the missing assumption).
- Refusals at the field: a feed of a field `eq` does not test (the handoff's Account hashing its
  display), a field fed twice or a non-template statement in either body (a hand-written
  comparator outside the template stays on `Hash`/`HashMap`), F64 (NaN is not equal to itself).
- Stale evidence cannot arise: the check reads the bodies the program has now, every build.
  Measured: whether this adds anything per instantiation (it should not: it runs per impl).

Open after slice 1 (review of 04989a2f): names in a recipe resolve in the type's module, so a
recipe cannot call its own module's private helpers, and a same-named function in the type's
module is called instead (documented; a hygienic expansion would need per-token module
spans). Text literals in recipes: done (2026-11-19, the written code shares the program's literal
table).

Out of this milestone: sums, ordering, disposal (closed derives stay), general CTFE.

## Proposal: sums (needs the user's call on recipe syntax; 2026-11-19)

The design (derive-laws.md) says a sum feeds "its tag and only the active payload". The built-in
`derive(Eq, Hash)` already writes sums (src/parser/derive.rs `sum_source`): `eq` as `match self`
then `match other` on the same variant, comparing payloads pairwise (`*mine0 == *theirs0 && ...`),
`_: return false`; `hash` folding the variant's index then each payload's hash. A recipe for sums
needs three things the record recipes do not:

- a loop over variants: `static for v in variants:`, with `v` the variant's name and `v.tag` its
  position (written as a number);
- the variant's binding pattern, which depends on its payload count: `v(mine)` written as
  `Dot(mine0, mine1):`, `Dot(mine0):` or `Dot:`;
- a loop over a variant's payload with an index joined to a name: `static for k in v.payload:`
  with `mine.k` written as `mine0`, `mine1` (a whole-word substitution cannot build `mine0`).

For example (proposed syntax):

```
pub derive Fed:
    impl HashKey for Self:
        fn feed(self: &Self, state: &mut HashState):
            match self:
                static for v in variants:
                    v(mine):
                        state.word(v.tag, 8)
                        static for k in v.payload:
                            (*mine.k).feed(state)
```

The key law would then read, for a sum: `eq` as the nested match above (each arm comparing every
payload position of the same variant, `_: return false`), and `feed` as one arm per variant that
feeds a distinct constant tag first and then a subset of that variant's payload positions which
`eq` compares; distinct tags make different variants' inputs differ only where that is harmless
(collisions are allowed; only equal-implies-equal-input is required). Nested `static for` would
be allowed for `variants` then `payload` only, so expansion stays bounded by the total payload
count. This extends the recipe template language beyond records, close to the parked general
`static for` question, so it waits for the user.

## Open: a recipe's names should resolve in the recipe's file (2026-11-19)

A recipe used in another file is expanded into impls whose methods belong to the type's module,
and the checker resolves a method's names by its module. So a name the recipe writes means what
the TYPE's file sees by it. Six review rounds (holes 3, 5, 2, 2, 2, 2) closed the ways that
differs by refusal (parser.rs `foreign_name`): a bare name declared elsewhere, or shared by the
recipe's file and the library, is refused. Each round finds another path (interpolations,
qualifiers, keywords that are declarable, library collisions), because the rule approximates
resolution lexically.

The structural fix: each token of an expansion carries its origin, the recipe's file for what
the recipe wrote and the type's file for what the derive substituted (`Self`, `For`, fields), and
the checker resolves each name by its token's origin, not the method's module. Needed: an origin
per name in the AST (or two-module visibility for a recipe's methods: the recipe's module, plus
the type and `for:` type named by the derive), private-name renaming (`Name@file`) applied to the
recipe body as in its own file, and parser-level resolution (module qualifiers, constants) by
origin. Then the lexical rule can go, and a recipe means the same in every file. Not started:
it touches the checker's name resolution, which the language direction should settle first.

## Proposal: `Self` in impl blocks (2026-11-19; a language-surface call, not made)

`Self` is the implementing type only inside a trait's declaration and in derive recipes (by text
substitution). In an `impl` block, inherent or of a trait, `fn helper(self: &Self)` is refused
("unknown type `Self`"), so a generator method cannot write it either (tenth review). Accepting it
would substitute the block's target (with its parameters) for `Self` in each method's parameter
and result types and in type annotations of its body, before methods are lifted, as recipes
already do textually. It touches the surface the book defines, so it waits for the user.
