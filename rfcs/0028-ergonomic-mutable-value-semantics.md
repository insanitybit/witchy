---
rfc: 0028
title: Ergonomic mutable value semantics — mutating-method statements, for var, and confined views
status: implemented
created: 2026-06-28
tracking:
---

# RFC-0028: Ergonomic mutable value semantics

The shipped mutable-iteration and confined-view lowering is implemented in
[`crates/witchy-syntax/src/parser.rs`](../crates/witchy-syntax/src/parser.rs),
[`crates/witchy-lower/src/escape.rs`](../crates/witchy-lower/src/escape.rs), and
[`crates/witchy-lower/src/codegen`](../crates/witchy-lower/src/codegen), with
counter evidence in [`src/stats.rs`](../src/stats.rs).

## Summary

Close the ergonomic gap that makes witchy's value semantics *feel* awkward next
to Go/Swift/Python, without adding references, aliasing, reference counting, or a
GIL. Three additions, all pure sugar or pure optimization over machinery that
already exists (the uniqueness pass and the RFC-0024 escape lattice), none
changing observable behavior:

1. **Mutating-method statements** — `xs.push(v)` as a *statement* mutates the
   receiver in place, instead of forcing `xs = list.push(xs, v)`. The completion
   of the RFC-0022 place-assignment sugar family.
2. **`for var x in xs:`** — mutable iteration: each element is bound mutably and
   written back, so you mutate elements in a loop without index ceremony and
   without references.
3. **Confined `View(a)`** — a zero-copy, read-only borrow into a contiguous
   buffer, valid only where the escape analysis proves it does not escape. Powers
   allocation-free `windows`/`chunks` and zero-copy read-iteration over large
   elements. (Escaping/returnable views are explicitly deferred — they need the
   lifetime work in [performance-modes.md](performance-modes.md) tier 4.)

## Motivation

witchy is **Hylo-style mutable value semantics**: you get mutation through `var`
/ `own` / in-place uniqueness, and you never get aliasing. That model is the
source of witchy's safety story (no shared mutable state → no data races, no
GIL, deterministic concurrency) and of its performance story (the uniqueness
pass turns `xs = list.push(xs, e)` into an in-place write). But three rough
edges make correct code read worse than it should, and push users toward asking
for references they do not actually need:

- **Accumulation reads as reassignment.** `nodes = list.push(nodes, x)` is the
  in-place fast path, but it *looks* like a functional rebuild. The natural form
  is `nodes.push(x)`. [RFC-0022](./0022-index-assignment.md) already gave `xs[i] = v` / `d[k] = v` /
  `acct.f = v` this treatment; the method-call mutator was simply left out.
- **You cannot mutate elements in a `for` loop.** `for x in xs: x.balance += 50`
  silently mutates a copy. The workaround — an index loop with `xs[i] = …` —
  works but is the kind of boilerplate that makes value semantics feel heavy.
- **Sub-ranges allocate.** A sliding window `xs[i..i+3]` or read-iteration over
  large records copies, so the obvious window/scan algorithms allocate O(n·k)
  where a borrowing language allocates nothing.

Each of these is repeatedly cited as "value semantics is awkward." None of them
actually requires giving up value semantics — they require finishing the
ergonomics of *mutable* value semantics. Crucially, the alternative the friction
suggests (references / `shared mut` / `Ref`) would reintroduce aliasing,
cycles, the GIL question, and a whole sharing type-system; this RFC gets the
ergonomics those features were wanted for while keeping the model that makes
witchy safe.

## Design

### 1. Mutating-method statements

Today `x.f(args)` is method-call sugar for `module.f(x, args)` as an
**expression** (spec §4/§12). This RFC adds a **statement** form:

> A method-call expression in **statement position** whose receiver is an
> **assignable place** and whose callee **returns the receiver's type** desugars
> to a place-reassignment of the receiver with the call's result.

```witchy
var nodes = []
nodes.push(Node(1, None))        // ≡ nodes = list.push(nodes, Node(1, None))
nodes.push(Node(2, Some(0)))

var tally = dict.new()
tally.insert("a", 1)             // ≡ tally = dict.insert(tally, "a", 1)
tally.remove("a")                // ≡ tally = dict.remove(tally, "a")

grid[i].push(cell)               // place receiver: ≡ grid[i] = list.push(grid[i], cell)
acct.history.push(entry)         // ≡ acct.history = list.push(acct.history, entry)
```

- **Assignable place** is exactly RFC-0022's set: a `var` binding, a subscript
  (`xs[i]`, `d[k]`), or a field (`a.b`), nested freely. The desugaring reuses the
  RFC-0022 place-write path verbatim, so the uniqueness pass turns it into the
  in-place mutation it already would for `nodes = list.push(nodes, …)`.
- **The return-type rule disambiguates.** A statement-position method whose
  callee returns the receiver's type writes back; one that returns a different
  type (`xs.length()`, `xs.first()`) is an ordinary expression-statement whose
  result is discarded, exactly as today. So nothing that reads as a query
  silently mutates.
- **Expression position is unchanged.** `let ys = xs.push(v)` still evaluates to
  the new list and leaves `xs` alone — value semantics. Only statement position
  on a place writes back, identical to the `xs[i] = v` (statement) vs
  `xs.set_at(i, v)` (expression) split that already exists.
- **User types get it for free**: a method that returns `Self` is a mutating
  method when called as a statement on a place — no new convention keyword.
- A method-call statement on a **non-place** receiver whose result is the
  receiver type and is discarded is a check-time warning ("result discarded;
  this does not mutate `f()`"), since there is nothing to write back.

### 2. `for var x in xs:` — mutable iteration

```witchy
for var acct in accounts:
    acct.balance += 50           // written back into accounts[i], in place
```

Desugars to an indexed loop with per-iteration write-back:

```witchy
for __i in 0..accounts.length():
    var acct = accounts[__i]
    acct.balance = acct.balance + 50
    accounts[__i] = acct          // RFC-0022 place write → in-place via uniqueness
```

- The iterand must be an **assignable place** of a list-like type; `for var` over
  a non-place (`for var x in f()`) or a lazy `Iter`/generator (no backing store
  to write to) is a check-time error naming the reason. Dict-value mutation stays
  `d[k] = …` for v1.
- **Write-back is loss-free across early exit.** `continue` writes back the
  current element before advancing; `break`/`return` write back the current
  element before transferring control — the same "mutations are never silently
  lost" rule that `var` *parameters* already follow ([`spec/language.md`](../spec/language.md) §7).
- `for x in xs:` (no `var`) is unchanged: read-only, and an assignment to `x`
  inside it stays the existing check-time error.
- Because it lowers to the indexed place-write both backends already implement,
  parity is automatic and the forced-copy differential oracle covers it.

### 3. Confined `View(a)` — zero-copy borrows that do not escape

A `View(a)` is a **read-only borrow into a contiguous buffer** (a `List(a)` or a
`String`'s bytes): a `(base, offset, len)` fat pointer into the owner's storage,
never a heap allocation. It is the sub-range analog of a `let`-borrow parameter.

```witchy
import list

fn sum3(nums: let List(Int)) -> Int:
    var best = 0
    for w in nums.windows(3):        // w : View(Int) — borrows into nums, no allocation
        let s = w[0] + w[1] + w[2]   // overlapping windows are READ-ONLY by nature
        if s > best:
            best = s
    best

fn main(console: Console):
    console.print("${sum3([1, 2, 3, 4, 5])}")
```

**Soundness — two invariants, both discharged by the [RFC-0024](./0024-unified-facts-lattice.md) escape lattice:**

1. **A view may not outlive its buffer.** A `View(a)`'s confinement must be ⊑ the
   scope of the value it borrows. Returning a view, storing it in a longer-lived
   structure, capturing it in an escaping closure, or sending it over a channel
   are check-time errors (*a `View` cannot escape its borrow scope*), enforced
   exactly like the existing `borrow_escape_check`.
2. **The buffer is immutable while a view of it is live.** For a view's live
   range the borrowed buffer is treated as a `let`-borrow — any in-place mutation
   of it in that range is a check-time error. (So you cannot hold a window and
   `push` to the same list at once; the lattice flags it.)

**API** (std/list, std/iter):

| Call | Result | Notes |
|---|---|---|
| `list.windows(xs, n)` | `Iter(View(a))` | overlapping, length `n`; read-only |
| `list.chunks(xs, n)` | `Iter(View(a))` | non-overlapping |
| `xs.view(lo, hi)` | `View(a)` | explicit sub-range borrow |
| `v[i]`, `v.length()`, `v.slice(lo, hi)` | element / `Int` / sub-`View` | sub-views compose |

These yield lazily through the existing `Iter`, so nothing is materialized.

**Zero-copy read-iteration (invisible).** `for x in xs:` where `x` does not
escape the body becomes a confined borrow rather than a per-element copy — pure
optimization, no syntax, decided by the same `confined_to ⊑ Frame` query. Free
win for loops over large records.

**Read-only in v1, by design.** Mutable *non-overlapping* views (`chunks_mut`
style) are sound but deferred; mutable *overlapping* views are unsound (they
alias) and are never offered — the same line Rust draws (`windows` yields `&[T]`,
never `&mut`). Element mutation in a loop is served by `for var` (§2), which
needs no view.

**Parity.** A `View` is a slice reference on the interpreter and a
`(base, offset, len)` triple into linear memory on WASM; both read identically.
Because views are read-only and confined, there is no observable aliasing to
diverge. The forced-copy differential mode may always materialize a `View` to a
real copy and must produce identical output — the soundness check, exactly as for
the in-place machinery.

### What is deferred

- **Escaping / returnable views** (return a window, store a slice past its
  buffer's scope). This is the one item needing genuine lifetime/region inference
  ([performance-modes.md](performance-modes.md) tier 4); confined views above do
  not. Mode-gate or defer.
- **Mutable views** (`chunks_mut`). Sound for the non-overlapping case; deferred
  until a workload needs it.

## Alternatives

- **References / `shared mut` / `Ref(a)`** (the path this RFC replaces). They
  deliver the same ergonomics but by adding aliasing, which brings back cycles,
  the GIL question, a non-`Send` boundary check, and a sharing type system —
  paying a large semantic cost for sugar that `for var` + mutating statements +
  confined views provide without it. The aliased-mutable-graph case that genuinely
  wants references is rare and served by arena-index handles; it should not drive
  the common-case ergonomics.
- **Do nothing.** The reassignment/index/copy forms are *correct* and already
  fast — but the persistent "value semantics is awkward" feedback is real, and it
  pushes users toward references for reasons that are purely surface-level.
- **A mutable-borrow parameter (`mut`) distinct from `var`.** Considered for §2's
  element binding; rejected because `var` already means "mutate and write back,"
  and `for var` is just that rule applied to a loop element. No new convention.
- **Make `for x` mutate by default.** Rejected: silent element write-back would
  be a footgun and would break the read-only iteration guarantee; mutation must be
  opted into with `var`, matching every other binding in the language.

## Drawbacks

- **More than one way to write an append** (`nodes.push(x)` vs
  `nodes = list.push(nodes, x)` vs `xs[xs.length()] = …`). Mitigated: `fmt`
  normalizes to the mutating-statement form where the receiver is a place, so the
  canonical surface is single.
- **The statement-vs-expression distinction for mutating methods** is subtle —
  `xs.push(v)` mutates as a statement but not as `let y = xs.push(v)`. This
  mirrors the existing `xs[i] = v` vs `xs.set_at(i, v)` split, but it is one more
  place the rule must be taught. The return-type-equals-receiver gate keeps it
  from ever silently mutating on a query.
- **`View` adds a type and a borrow-scope check** to the surface and to typeck.
  Kept minimal by making views read-only and non-escaping in v1, so the check is
  the existing no-escape rule, not new lifetime machinery.
- **`for var` early-exit write-back semantics** must be specified and tested
  carefully (write back before `break`/`return`), or a mutated element could be
  lost on early exit. Covered by the same tests that pin `var`-parameter
  write-back.

## Prior art

- **Hylo (Val)** — mutable value semantics, `inout`/`sink`/`let`, mutation
  without references; `for var` and the mutating-method statement are its loop and
  method ergonomics. witchy's conventions already descend from this.
- **Swift** — value types with in-place element mutation via `subscript` and
  index iteration; `Array` mutating methods (`append`). The `nodes.push(x)`
  statement is the witchy spelling of Swift's mutating methods.
- **Rust** — `slice::windows`/`chunks` returning `&[T]` (read-only for
  overlapping) is the exact model for confined `View`; `chunks_mut` is the
  deferred non-overlapping mutable case.
- [RFC-0022](./0022-index-assignment.md) (place assignment — the sugar family this
  completes), [RFC-0024](./0024-unified-facts-lattice.md) (the escape lattice that
  discharges view soundness), [ownership-analysis.md](ownership-analysis.md) (the
  in-place machinery the statements lower onto).

---

> 2026-06-28: **`for var` v1 landed.** Implemented as a parser desugar to an
> indexed range-loop with a `xs[i] = x` place write-back (so both backends lower
> it identically and the uniqueness pass keeps it in place); restricted to a
> single loop variable over a plain list variable. v1 rejects a `break`/
> `continue`/`return`/`?` that belongs to the loop (a compile error, not a silent
> lost write) — straight-line element mutation only; loss-free write-back across
> early exit, plus `nodes.push(x)` mutating-method statements and confined
> `View`s, remain. RFC stays `proposed` until all three ship.

> 2026-06-28: **`nodes.push(x)` implementation constraint (for the next increment).**
> A parse-time desugar of a statement-position `place.method(args)` to
> `place = place.method(args)` is **unsound**: a bare `xs.length()` statement
> (discarding an `Int`) is legal today, and the desugar would turn it into the
> type error `xs = xs.length()`. The "callee returns the receiver's type" gate
> therefore needs the method's return type AND the UFCS receiver-type resolution,
> neither of which exists pre-typeck — and `typeck::check` is read-only
> (`&Module`). So this feature must be a **typeck-integrated rewrite**: during
> typeck's existing typed walk, collect each statement-position method-call site
> whose receiver is an assignable place and whose resolved return type equals the
> receiver type, then apply the place-reassignment rewrite to those sites before
> codegen/interp (one shared AST edit → parity by construction, exactly like the
> `for var` desugar). Confined `View`s (feature 3) stay blocked on RFC-0024's
> escape lattice.

> 2026-06-29: **Confined views v1 landed — as invisible copy-elision, not a new
> `View` type.** Feature 3 ships in its first and most impactful form: a
> `let w = list.slice(src, lo, hi)` that the RFC-0024 escape analysis proves
> confined (read ONLY via `at`/`length`, with `src` never reassigned/mutated nor
> used as a whole value — so no alias can mutate the borrowed buffer) compiles to
> a zero-copy borrow — `w` keeps `src`/`lo`/`hi` and reads through them via the
> `$list_at_view`/`$list_len_view` helpers, which recompute the clamped window and
> trap on the view bound so every read matches the interpreter reading the
> materialized copy. No `View(a)` type, no `.view()`/`windows`/`chunks` surface,
> no borrow-scope check were added: the same zero-copy result is delivered as an
> optimization of the existing copy-based `list.slice`, gated `WITCHY_OPT=views`
> (default-on) and proven by a `witchy stats` heap-drop counter + the differential
> de-opt sweep. **Deferred to a follow-up:** extending the same view machinery to
> `windows`/`chunks` producers (they yield `Iter` of windows, a different shape),
> and the explicitly out-of-scope escaping/returnable and mutable views. With all
> three features (mutating-method statements, `for var`, confined views) now
> landed in v1 form, this RFC moves to `implemented`.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
