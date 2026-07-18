---
rfc: 0025
title: frozen — deep immutability, the dual of uniqueness
status: implemented
created: 2026-06-28
tracking:
---

# RFC-0025: `frozen` — deep immutability, the dual of uniqueness

The shipped `frozen` qualifier and its mutability checks are implemented in
[`crates/witchy-syntax/src/parser.rs`](../crates/witchy-syntax/src/parser.rs) and
[`crates/witchy-types/src/typeck.rs`](../crates/witchy-types/src/typeck.rs), with
qualifier coverage in the type-checker tests.

## Summary

Add a `frozen` qualifier asserting a value is **deep-immutable forever**. Where
the uniqueness story proves *"only I can see this, so mutate it in place,"*
`frozen` proves the symmetric fact: *"no one will ever mutate this, so share it
by reference."* A `frozen` value can cross witchy's value-semantics copy
boundaries — closure capture, task message, default argument — **without being
copied**, with no reference count, reclaimed with its arena/region like any other
allocation. It is the missing half of witchy's ownership model and directly
attacks witchy's dominant remaining tax: the copy-at-every-boundary that value
semantics demands.

## Motivation

witchy's value semantics are the source of its safety (no shared mutable state,
no locks, deterministic concurrency) and of its cost: **every boundary that
carries a value out of a scope carries a copy** (§7 of the language spec). The
uniqueness pass recovers the *mutation* cost (in-place push instead of
copy-per-push) but does nothing for the *sharing* cost. Today:

- A closure that captures a 10 MB lookup table copies all 10 MB at capture.
- Sending an immutable config to 100 worker tasks copies it 100 times.
- A default argument that is a large constant table copies per call.

Yet in every one of these cases the value is *read-only* — nothing mutates it
after construction. The copy buys nothing. The only reason witchy copies is that
it cannot, in general, prove the value won't later be mutated through some alias.
`frozen` is the programmer supplying that proof.

This is precisely the gap Ante reaches for `Rc` (shared *mutable*) to fill, and
why Ante then needs a runtime borrow check. witchy never needs shared
*mutability* — its concurrency and closures are already copy-isolated — so the
cheaper, sound primitive is shared *immutability*. `frozen` gives witchy the
zero-copy sharing of an RC/GC language **without** the refcount word or the
collector, because deep immutability + arena lifetime makes aliasing observably
indistinguishable from copying.

## Design

### The qualifier

`frozen` applies in the same positions as the ownership conventions:

```witchy
let table: frozen Dict(String, Int) = build_table()   // binding
fn lookup(t: frozen Dict(String, Int), k: String) -> Option(Int)   // parameter
type Atlas frozen:                                     // type: every value is frozen
    tiles: List(Tile)
```

A `frozen T` is a `T` with one removed capability and one added: it can never be
the target of in-place mutation (`var`, `own`, `xs[i] = v`, `s = s <> p`,
`d[k] = v`) — those are check-time errors — and in exchange it is **exempt from
copy-out at value boundaries**.

### Freezing is a one-way coercion

A value enters the frozen world exactly once and never leaves:

```
T          ──freeze──▶   frozen T        (one-way; the seal)
unique T   ──freeze──▶   frozen T        (cheap: the unique value is sealed in place)
frozen T   ──/──▶        T               (no thaw; would reintroduce the mutation
                                          a sharer might observe)
```

- From an ordinary (possibly `Shared`) value, `freeze` is a deep copy *then*
  seal — you pay one copy to enter, then never again. The interpreter models this
  as a plain deep copy (value-neutral); the compiled backend, when the source is
  `Unique` (per [RFC-0024](./0024-unified-facts-lattice.md)), seals in place with zero
  copy.
- `frozen` is **deep**: `frozen List(Atlas)` guarantees the list *and every
  element and their fields* are immutable, so a borrow into any depth
  (`atlas.tiles[3]`) is shareable. This is what makes capture-by-reference sound
  all the way down — and it is the structural reason witchy does not need Ante's
  recursive reachability check: immutability is a property of the *value*, sealed
  at the boundary, not a relationship the compiler must re-derive between types.

### What it unlocks (consumers of the escape oracle)

Each is a pure consumer of [0024](0024-unified-facts-lattice.md)'s facts; none
changes observable behavior (a frozen value shared by reference is
indistinguishable from one copied, *because* it is immutable — enforced by the
forced-copy differential mode, which may always fall back to copying a frozen
value and must produce identical output).

1. **Zero-copy closure capture.** A closure capturing a `frozen` value captures
   the pointer, not a copy. (Today: capture-by-value copy.)
2. **Zero-copy task messages.** `chan.send(tx, frozen_value)` transfers a
   pointer; all receivers share one immutable buffer. The "one message type per
   program" model is unaffected; this is purely how the payload is marshaled.
3. **Zero-copy default arguments** and module constants: a top-level `let` of a
   large literal table becomes a single shared frozen allocation.
4. **Interning / literal dedup.** Two structurally-equal frozen literals can
   share one allocation (sound only because neither can mutate).

### Lattice integration

`frozen` adds one absorbing state to the uniqueness lattice of
[0024](0024-unified-facts-lattice.md):

```
Unique  ⊑  Shared  ⊑  Dead
   │
   └────▶ Frozen        (sealed: shareable, never mutable, never `Dead` early)
```

`Frozen` is reached only by `freeze` and, once reached, a value is never moved or
in-place-mutated, so it sidesteps the `__cap` machinery entirely. Reclamation is
unchanged: a frozen value is freed with its enclosing arena/region/watermark; it
just may have many readers until then.

### Reclamation note

Because a frozen value can be shared past the scope that built it (e.g. captured
by a spawned task), its confinement (per [0024](0024-unified-facts-lattice.md))
is the *join* of all its sharers' scopes. The arena/watermark reclaim must see
that join — a frozen value shared into a longer-lived task is `confined_to` that
task, not the builder's frame. This is the same escape query everything else
uses; `frozen` does not need its own reclamation mechanism, only an honest escape
level. (It explicitly does **not** introduce reference counting — picking one
memory identity, per [RFC-0016](./0016-reference-counted-memory.md), stands.)

## Alternatives

- **Reference counting (RFC-0016).** RC also gives shared values, but at a
  refcount word per object and atomic-or-not bookkeeping, and it is the memory
  identity witchy's RFC-0016 explicitly weighs *against* the arena/linear bundle.
  `frozen` gets the sharing win for read-only data without adopting RC at all —
  the value is reclaimed in bulk with its arena. They are not mutually exclusive,
  but `frozen` covers the common case (immutable shared data) that is most of why
  one would want RC.
- **Infer immutability instead of declaring it.** The compiler can already prove
  some values are never mutated. But sharing-by-reference is observable through
  *timing/memory*, not values, so inferring it silently is fine — and we should
  do it where provable. `frozen` is for the cases inference can't reach
  (especially across the task-message and `pub` API boundaries, where the
  summary is the contract) and to let a library *promise* zero-copy sharing as
  part of its type.
- **`const`/`comptime` only.** Compile-time constants are already shared, but
  `frozen` covers *runtime-built* immutable data (a table loaded from a file,
  then frozen and fanned out), which comptime cannot.

## Drawbacks

- A second qualifier family alongside `let`/`var`/`own` is more surface area to
  learn. Mitigated by the clean mental model — `frozen` is the exact mirror of
  `own`/`unique`, and the two are taught as a pair (mutate-in-place vs
  share-by-reference).
- The one-way coercion means a value frozen too eagerly cannot be cheaply
  mutated later (you must copy out of the frozen world). This is inherent to the
  guarantee; the fix is to freeze at the right point, which `mode opt` can flag.
- Deep immutability interacts with generics: a `frozen List(a)` requires `a`
  itself to be freezable. This is a bound (`a: Freeze`-shaped), resolved by
  monomorphization like other bounds.

## Prior art

- [Ante: blending borrowing and reference counting](https://verdagon.dev/blog/ante-blending-borrowing-rc)
  (Evan Ovadia, 2026) — motivates the shared-immutable vs shared-mutable split;
  witchy takes the shared-immutable half that needs no runtime check.
- Vale's pure functions — immutable borrows into shared-mutable data; `frozen` is
  the value-semantics analog (immutable by construction, not by region-pure
  windows).
- Clojure's persistent immutable values; Swift `let` deep-immutability — the
  "immutable ⇒ freely shareable" intuition, here made a compiler-exploitable
  fact rather than just a guarantee.

---

> 2026-06-29: **Implemented — as a CONTRACT, the optimization being subsumed by
> witchy's existing value semantics.** `frozen T` (and `unique`/`local unique`, see
> [RFC-0026](./0026-unique-qualifier.md)) parse as a `Type::Qualified` qualifier, format/round-trip, thread through
> generics/aliases/traits, and lower to the inner type (no runtime representation →
> parity-neutral). Enforcement (RFC-0025's teeth): a `frozen` value is deeply
> immutable, so the checker rejects declaring one mutable — `var x: frozen T` and a
> `var`/`own` frozen parameter are type errors. The deeper transitive guarantee (a
> frozen value's fields are never mutated through any alias) is ALREADY provided by
> witchy's value semantics + uniqueness inference: a shared value is never mutated in
> place (the `__cap`/uniqueness pass copies first when a buffer is aliased), so there
> is no aliased-mutation to forbid.
>
> The zero-copy SHARING this RFC sought (closure capture, task messages, default
> args, interning) was VERIFIED to already happen: closures capture heap values by
> pointer (`W::ToSlot(GetLocal …)`, no deep copy) and `let y = x` shares the pointer
> — so there is no measurable copy for `frozen` to elide (it would be a no-op lever,
> deliberately NOT added to the `WITCHY_OPT` registry per RFC-0030's no-phantom-lever
> rule). `frozen` therefore ships as a compile-time CONTRACT / API-expressiveness
> feature, not a performance optimization — its perf goal is met by construction.
> Marking implemented.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
