# Proposal: invariants over a container's elements

Status: implemented (2026-11-25). Written after reading `scalar_vc/`, `contract.rs`,
`check/invariants.rs`, `memory_model.rs`, `memory_contract.rs`, `record_contract.rs`,
`library/wasm-vec.wy` and the Lean prelude (`proof_prelude.lean.in`; generated proofs import
Lean core and `Init.Omega` only, not `proof/Witchy/Memory*.lean`). The code is
`scalar_vc/elements.rs`; the fixtures are `tests/fixtures/element-invariants/`.

## The gap

A sealed type may state an invariant over its number fields and over a field's length
(`items.len()`). It cannot say anything about the values a `Vec` field holds:

```
sealed type Index:
    bytes: Vec<U8>
    spans: Vec<Range>
    invariant forall i in (0, spans.len()): spans[i].start + spans[i].len <= bytes.len()
```

Today this is refused in every proved function of the file: `forall` parses, but the scalar
clause grammar has no case for it. So a read through a stored offset keeps its run-time range
test, and a `nopanic` reader has to carry an `Err` that can never happen.

## What exists already

- **Cells have values in the memory and record models.** `memory.cell(buffer, i, item)`,
  `memory.unchanged_except(buffer, old(buffer), i)` and `memory.equal_prefix(buffer,
  old(buffer), n)` are proved by Lean for `push_spare_mut`, `take_last_mut`, `replace_mut`,
  `reserve_mut`, `get` and `at`.
- **The scalar model drops them.** `Claims::memory` and `Claims::record` keep the number
  comparisons of a callee's contract and discard every clause about cells.
- **`push_mut`, `index_set`, `set` and `swap` are proved in the scalar model** from their
  callees' contracts, so they state lengths only.
- **An element read out of a Vec gets no facts.** `tests/fixtures/invariants/from-vec.wy`
  passes because `deposit` assumes the invariant of its own `&mut Account` parameter, not
  because the read carries it.

## Design

The scalar model learns three things. No quantifier reaches Lean: each obligation is stated
for one arbitrary element or one arbitrary index, which is a fresh variable to `omega`.

### 1. The clause

`invariant forall i in (0, v.len()): P` on a sealed type, where `v` is a field, every use of
`i` is `v[i]`, and `P` is a scalar clause over number fields of `v[i]` and of the type itself.

- `v[i]` resolves through the source as written, never by a type's name: the type's
  `index(c, i)` is `return c.m()[i]`, and `m` is `return memory.slice_unchecked(&c.B, 0, c.L)`.
  The elements are then the cells of buffer `B` below length `L`, and the bound `v.len()` must
  resolve to the same `L`.
- In the resolved clause an element field is a path through the buffer and one marker step
  (`ANY_ELEMENT`); every other field is one of the type's own.

### 2. What a call does to elements

A callee's contract is read for its effect on each buffer reached through a `&mut` argument
(`scalar_vc/claims.rs`):

| Clause in the callee's contract | Effect |
|---|---|
| `memory.unchanged(b, old(b))` | every cell kept |
| `memory.unchanged_except(b, old(b), i)` | every cell but `i` kept |
| `memory.equal_prefix(b, old(b), n)`, `memory.same_prefix` | cells below `n` kept |
| `memory.cell(b, i, x)`, `x` a parameter | cell `i` now holds `x` |
| `memory.cell(b, i, r)`, `r` the result | the result is the element at `i` |
| `memory.cell(old(b), i, r)` | the result was the element at `i` (`take_last_mut`) |
| none of these | unknown: the call is refused |

Those are shown by Lean in the memory and record models. A function proved in the scalar
model states the same effects itself, with `elements.` clauses among its `ensures`:

| Clause | Says |
|---|---|
| `elements.kept(b)` | every cell kept |
| `elements.kept_below(b, n)` | cells below `n` kept |
| `elements.kept_except(b, i)` | every cell but `i` kept |
| `elements.holds(b, i, item)` | cell `i` holds the parameter `item` |
| `elements.among(b, n)` | every value below `n` was a value below `n` before (`swap`) |

`b` is a `Buffer` field of a `&mut` parameter. The walk shows each clause at every return
from a log of what the body did to `b`, in order: the effects of the calls it made, and
stores through a reference to one cell. Lean shows the index facts (an index the clause says
is kept is one no event reaches; the index stored at is the one stated; a value moved came
from below `n`). Where two ways join, each way's events hold under that way's condition. A
loop, a store of the whole value, or a call that says nothing ends what can be shown.

Three shapes are read from a callee's body as written, never from its name:

- `index(c, i)` is `return c.m()[i]` and `m` is `return memory.slice_unchecked(&c.B, 0, c.L)`;
- a cell lender (`at_mut`) is `if test: return Ok(memory.mut_unchecked(&mut c.B, i))` then
  `return Err(k)`, with no call in the test: its `Ok` changes nothing and refers to cell `i`
  alone. The reference counts as a store of a value only when the arm that binds it starts
  with `*place = value` and uses the binding nowhere else; otherwise cell `i` is unknown;
- a function whose body is one `unreachable` never returns, so the path that calls it ends.

Vec states: `push_mut` (`Ok`: kept below the old length, the new cell holds the item; `Err`:
kept below the length), `index_set` and `set` (kept except `index`, which holds the item),
`swap` (`among` the length). `take_last_mut`, `push_spare_mut`, `replace_mut`, `reserve_mut`,
`get` and `at` already said it in memory clauses.

### 3. Keeping and using the invariant

In a function of the type's file, for each parameter of the type:

- **Assumed on entry**, as every type invariant is.
- **Kept at every change**, not only at the return, so it can be used at any read:
  - a call that is lent the buffer must have a known effect. For each `Result` arm, under that
    arm's facts, Lean shows that every index below the new length is a kept index below the
    old length or one of the cells written, and that `P` holds of each value written (which
    is passed by name, so its fields are followed);
  - a change to one of the type's own fields that `P` reads, by an assignment or a call,
    must keep `P` for an arbitrary element (`P(e, old) -> P(e, new)`). So text a span points
    into may grow, and may not shrink, whatever the spans are;
  - anything else that can reach the buffer, the count or those fields (a kept `&mut`, a
    store of the whole field, an exchange) is refused.
- **Built** only with the field empty: every function that returns the type shows
  `v.len() == 0` of what it returns, which makes the invariant true of no elements. A value
  that has elements is changed in place, through `&mut`.
- **Used at a read**: a value read by a call whose contract says `memory.cell(b, i, result)`
  (`at`, `get`, `take_last_mut`), by `v[i]`, or from a slice local made by the counted view
  (`let s = v.as_slice()`, `s[i]`), gets `P` of its fields as a fact.

A type may state several invariants over one field (`import toml` has one for a node's key
span and one for its text span). A read gets every one stated over the same cells with the
same count; each is shown separately at every change.

A function elsewhere never sees the fields, so it reads elements only through the type's own
methods, whose `ensures` carry what they need.

In a `nopanic fn` every slice read asks Lean for `index < length` (before, only a read
guarded by that very comparison did): such a function is refused anyway if the read keeps
its check, and this is what lets a fact like `start + len <= text.len()` remove it.

### Limits

- The index is used only as `v[i]`, the range is `(0, v.len())`, and `P` is a scalar clause:
  no relation between two elements (sortedness), no nested `forall`.
- An element is a record of numbers; `P` reads its number fields.
- No function returns a value of the type with elements, and none shrinks a field `P`
  bounds elements by.
- A refusal for a change the invariant does not follow is placed at the invariant, not yet
  at the statement.

### Reviewed and left as written

Three rounds of independent review attacked this; the holes found are in
`~/workspace/witchy-findings/` and fixed. The last round accepted nothing wrong and noted
these, none reachable today:

- a store through a tracked field keeps the tag of the value it replaced until the next
  change clears it;
- forgetting a root does not clear the slice locals made from it (a slice local cannot
  outlive the lend that made it);
- a lend of the count alone is not checked for coverage, and a view's own count is ignored
  (a count must be `field.len()`, refused otherwise at the declaration);
- a read through a call's result takes every invariant over the buffer, each guarded by its
  own count, where `v[i]` takes those with the same count.

A fourth round, on the merged tree (74 probes: writes from outside the type, shrinking the
sibling Vec, Vec mutators with no element clause, own fields lowered thirteen ways, rebuilding
with elements, stale and forged read facts, false `elements.` clauses on a user container),
accepted nothing wrong. It read two more points, neither reachable: the element readers match
a view by its buffer and not its count (the memory model refuses a second view whose count is
not the prefix length), and `Claims::memory` takes a stored parameter from `memory.cell`
without the borrow check `Claims::record` has.

A fifth pass took what the fourth did not reach, the caller's side of `swap` and
`take_last_mut`: thirteen probes (a taken or read span used against another value's text, a
strict bound claimed of a taken or swapped span, a taken span pushed back longer, a count or
slice kept across the take, a write after a swap). The ten wrong ones were refused and the
three right ones accepted; three are kept as fixtures.

An independent reviewer then took the same ground, the caller's side of the Vec effects
(about 95 probes: facts crossing two values of the type and whole-value exchanges, facts
crossing match arms and joins, stale indexes after a take, wrapping sums in narrow fields, a
clause that reads its own count, read facts leaking out of an unevaluated path). Nothing wrong
was accepted.

Not covered by a fixture: a memory invariant and a `forall` on one type, a generic
`Index<T>`, and a `for i in 0..v.len()` loop that pops.

## What is trusted

The same things as the rest of the scalar walk: which statements can change a place, the
reading of a callee's contract, and the shape checks above. New in this proposal: the effect
table, and the composition of effects when a scalar-proved function states its own. Lean
checks every arithmetic step, each `P` of a written value, the coverage of indexes, and the
callee contracts the effects are read from.

## Bit scans

`U64.trailing_zeros(x)` and `U64.count_ones(x)` are word intrinsics beside `U64.mul_high`:
`i64.ctz`/`i64.popcnt` on Wasm, `ctz`/`popcnt` in Cranelift, admitted by both host opcode
lists, no trap. The scalar walk knows each is at most 64, that a nonzero value below `2^k` has
fewer than `k` trailing zeros (k = 8, 16, 32, 64), and that a byte-lane bitmask is below 65536.
`Span`'s native byte search returns the lane the mask names. Fixtures: `tests/fixtures/bitscan/`.

## Proof time

`tests/proof-budget/vec-only.wy`, empty cache, seven alternating pairs of the compiler from
before any Vec change and this one, on a loaded machine: prover median 6.02 s before, 6.42 s
after (+6.6%, inside the 10% budget). The `elements.` obligations join each function's
existing body goal, so they add no goal.

Measured again on the branch that added `import toml`, the same program took 9.7 s (+52%):
the UTF-8 validator every program carries had been given a proof so that a `nopanic fn` may
validate text, and that one goal took 2.5 s. Its per-scalar step is now a function of its
own; the two proofs together take no time that can be measured, and seven pairs give 6.87 s
before and 7.04 s after (+2.5%; 269 goals against 266).
