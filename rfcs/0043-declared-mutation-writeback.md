---
rfc: 0043
title: "Declared mutation: write-back by declaration, not inference"
status: implemented
created: 2026-07-03
predecessors:
  - "0028 (ergonomic mutable value semantics — the statement write-back this re-grounds)"
  - "0022 (index assignment — the place-assign desugar family)"
  - "scratch/full-evaluation-2026-07-03.md Theme A + consistency-analysis §4 (the evidence)"
tracking:
---

# RFC-0043: Declared mutation — write-back by declaration, not inference

> Provisional snippets; code blocks are deliberately **not** tagged `witchy`
> so the doc-examples sweep does not execute pre-implementation code.

## Summary

RFC-0028's mutating-method statement (`xs.push(1)` as a statement reassigns
`xs`) is today decided by `rewrite_mut_method_stmts`
([crates/witchy-syntax/src/linker.rs](../crates/witchy-syntax/src/linker.rs):631-678): a **link-time, name-global,
receiver-type-blind census** over every function in the linked program. Two
probed, silent failure classes follow directly from that mechanism. This RFC
replaces the census with a **declaration**: a function opts its receiver into
write-back with the existing `var` parameter convention
(`fn push(var xs: List(a), x: a) -> List(a)`), resolution happens per
receiver type after method resolution (no name census), and a
statement-position method call that is *not* write-back eligible and whose
result is non-Nil becomes a **compile error** — killing the silent-discard
class outright.

This RFC is one of a set (0042–0055) with one thesis, which CLAUDE.md already
states as law for optimizations: **facts must live in local, typed
declarations — not global censuses, allowlists, or string heuristics.** Here
the fact is "this operation mutates its receiver"; today it is inferred from
a whole-program name census; after this RFC it is written in the signature.
[RFC-0046](0046-typed-trait-dispatch.md) applies the thesis to dispatch,
[RFC-0050](0050-method-call-generalization.md) to method syntax itself.

## Motivation

### The current rule, precisely (linker.rs, verified 2026-07-03)

After linking and constant folding, `rewrite_mut_method_stmts` (:631) rewrites
`place.method(args)` in statement position into
`place = place.method(args)` when **all** of:

1. `place`'s base variable is a `var` binding or `var` parameter
   (`place_base_is_mutable`, :698-706); the tail statement of a value-position
   block is excluded (:721);
2. the bare method name is **not declared by any `impl` or `trait` anywhere in
   the linked program** (the `shadowed` set, :634-649);
3. **every** free function with that bare name — from any module in the link
   set — is "self-returning": its first parameter's declared type textually
   equals its return type (`fn_is_self_returning`, :687-692; the generic
   `List(a) -> List(a)` qualifies).

Every condition is global and type-blind: the decision for `xs.push(1)` on a
*List* consults the names of methods on unrelated types and the signatures of
unrelated modules' functions, and never asks what type `xs` is.

### Failure 1 (probed): a dependency's method name flips your semantics

```
type Bag:
    n: Int

impl Bag:
    fn push(self, v: Int) -> Bag:
        Bag(self.n + v)

fn main(console: Console):
    var xs = []
    xs.push(1)
    console.print("${xs}")      # prints []  — silently, no warning
```

Adding this `impl Bag` — anywhere in the linked program, including inside a
dependency you just upgraded — puts `push` in the `shadowed` set, so the
unrelated `xs.push(1)` on a List silently stops writing back and becomes a
discard. Probed: output `[]`; `witchy parity` confirms both backends agree,
so parity machinery can never catch it. Delete the impl and the same line
prints `[1]`. One name declared in one file flips the mutation semantics of
every same-named method statement in the program.

### Failure 2 (probed): filter mutates, map no-ops — same syntax

Eligibility keys on the *generic declared* return type equalling the receiver
parameter's type. `list.filter : List(a) -> List(a)` is "self-returning";
`list.map : List(a) -> List(b)` is not. So, probed:

```
var xs = [1, 2, 3, 4]
xs.filter(fn(n: Int) -> Bool: n > 2)   # xs is now [3, 4]  — MUTATES
var ys = [1, 2, 3]
ys.map(fn(n: Int): n * 10)             # ys is still [1, 2, 3] — no-op
```

Same syntax, opposite effects, invisible at the call site, no diagnostic on
either backend. And the rule mis-fires *forward*: any future builder-style
API — `router.route("/health")` returning `Router` for chaining — is
self-returning by shape, so its statement form silently becomes a mutation
the author never designed. The signature shape is a proxy for intent; only
the author knows the intent; so the author must declare it.

## Design

### 1. The declaration: a `var` receiver

A function declares itself a **mutator** by giving its first parameter the
`var` convention *and* returning that parameter's type:

```
pub fn push(var xs: List(a), x: a) -> List(a):
    __list_push(xs, x)
```

Reading: "this function's result is *the receiver, updated* — its statement
form writes back." The three call forms:

- **Expression position** — `let ys = xs.push(1)` / `list.push(xs, 1)`:
  an ordinary value-semantics call. Any argument is accepted (`let`, `var`,
  a literal, a call result); nothing is written back through the parameter.
  The mutation is delivered through the return value, as it always was.
- **Statement position on a mutable place** — `xs.push(1)` where `xs`'s base
  is a `var`: desugars to `xs = list.push(xs, 1)`
  (`desugar_place_assign`, parser.rs:1913). Nested places compose as today:
  `accs[0].items.push(2)` becomes the nested place-assign.
- **Statement position on an immutable place / non-place** — compile error:
  ```push` mutates its receiver — declare `xs` as `var`, or bind the result
  (`let ys = xs.push(1)`)``.

### 2. `var` is overloaded — deliberately, and checked

What `var` on a parameter means today ([spec/language.md](../spec/language.md):361-392, probed):
the argument must be a plain mutable `var` at the call site, the callee may
reassign the parameter, and the caller's variable is written back — even on
early return; `move` into `var` is rejected. That is a *procedure-style*
channel (`fn bump(var n: Int):`).

This RFC splits `var`'s meaning by the function's shape, enforced by the
checker so no ambiguous case exists:

| shape | meaning |
|---|---|
| `var` param, function returns `Nil` | **procedure channel** — today's semantics unchanged: call-site arg must be a mutable `var`, param write-back applies |
| `var` **first** param, return type == that param's type | **mutator declaration** — expression form is pure (any arg, no param write-back); statement form writes back per §1 |
| `var` param in any other position/shape | **compile error** — "a `var` parameter must be a write-back channel (return `Nil`) or a mutator receiver (return the receiver's type); split the function or return a tuple" |

Why overload rather than mint a fourth convention keyword: the spec's own
definition of `var` — "the callee mutates and the caller's variable is
written back" — describes the mutator's *statement form* verbatim; the two
readings are disjoint by signature shape (Nil vs self-typed return), so a
reader never has to guess which applies; and a new keyword (`mutating`,
`inout`) would put two spellings of "writes back" in the conventions table
forever. The real cost of the overload is that the *call-site mutability
demand* differs between the two shapes (procedures demand a `var` argument;
mutators demand it only in statement form) — the checker's error messages
carry that distinction, and it is the correct distinction: an expression-form
mutator call visibly redirects its result, so the source binding is not a
write-back target.

**Breaking change, called out:** today a `var` parameter on a
value-returning function has *combined* semantics — probed:
`fn push_twice(var xs: List(Int), x: Int) -> List(Int)` both writes back
through the parameter *and* returns the value (`xs=[1,9] ys=[1,9,9]`). That
dual channel is exactly the confusion this table removes. No std or examples
code uses the combined form (grep: zero `(var ` parameters in std/); user
code that does gets the row-3 compile error. Break-don't-deprecate.

### 3. Resolution: per receiver type, after method resolution

The rewrite moves out of the linker (delete :631-678 and its call at :616)
into the method-resolution pass in `crates/witchy-types/src/traits.rs`, at
the point where `place.method(args)` is resolved to a concrete function —
impl method, trait method, or UFCS module function — with the receiver's
type known (today's receiver typing; [RFC-0046](0046-typed-trait-dispatch.md)
makes it total, [RFC-0050](0050-method-call-generalization.md) widens which
receivers resolve). The statement-form decision then reads the **resolved
callee's declaration**:

- resolved to a mutator (§2 row 2) and the place is mutable → write-back
  rewrite;
- resolved to a mutator and the place is immutable → error (§1);
- resolved to anything else returning `Nil` → plain statement, as today;
- resolved to anything else returning non-Nil → **error**: ``result of
  `map` is discarded — bind it (`let ys = xs.map(f)`), reassign
  (`xs = xs.map(f)`), or discard explicitly (`let _ = xs.map(f)`)``.

`let _ =` is the explicit discard escape hatch (it already works, probed).
Bare *call*-form statements (`list.push(xs, 1)` as a statement) do **not**
write back — the receiver of a method call is syntactically the target; the
first argument of a free call is not — they fall under the discard error,
which names the method form as the fix.

Both failure classes die by construction: shadowing is impossible (the
decision consults only the resolved callee, and per-type resolution means an
`impl Bag: fn push` never intercepts a List receiver), and filter/map both
stop being silent (`filter` without a `var` receiver becomes a discard error,
not a mutation; the author of a builder API simply doesn't write `var`).

### 4. Parity and the optimizer

Parity by construction, unchanged from RFC-0028: the rewrite edits the single
linked AST both backends consume, before either lowering. The uniqueness
pass keeps seeing the exact self-assign shape (`xs = list.push(xs, 1)`) it
already optimizes to in-place mutation, so the perf story is untouched. The
only pipeline change is *when* the rewrite runs (post-method-resolution
instead of post-link), and the produced `Assign` statements are re-checked by
the final check pass that already follows trait lowering.

### 5. std migration (the `var` receiver list)

The test for granting `var`: **the result is the receiver, updated** — same
collection/value identity, contents changed (insert/remove/reorder/
replace-element/normalize). Transformations that build a *different* value of
the same type stay `let` and become discard errors in statement form.

- `list`: `push`, `concat`, `sort`, `sort_by`, `reverse`, `set_at`,
  `update_at`. (Not `map`/`filter`/`take`/`drop`/`slice`/`flatten`/… —
  transformations; `xs.filter(p)` in statement form is the new error, the fix
  for Failure 2.)
- `dict`: `insert`, `remove`, `update`, `merge`. (`dict.set_at` is deleted by
  [RFC-0049](0049-naming-lexicon.md); the `d[k] = v` desugar retargets to
  `insert`.)
- `set`: `insert`, `remove`. (The algebra — `union`/`intersection`/
  `difference` — stays `let`.)
- `string`: `replace`, `replace_first`, `to_upper`, `to_lower`, `trim`,
  `trim_start`, `trim_end`, `pad_left`, `pad_right`, `center`,
  `strip_prefix`, `strip_suffix` — the normalize-in-place set, so
  `var s … s.trim()` keeps working. (Not `repeat`/`take`/`drop`/`split`.)
- Everything else in std keeps plain receivers; each borderline call above is
  a reviewable line in the migration diff, and the signature in
  spec/stdlib.md (generated from these sources) becomes the user-visible
  documentation of which form a function is — the fact, in the declaration,
  where the docs are rendered from.

Behavioral deltas to pin in differential tests: today-eligible names that
lose write-back (`filter`, plus any self-returning user function not opting
in) now *error* rather than silently flipping; today-shadowed names that
gain it (a List `push` statement in a program that also has an `impl … push`)
now correctly write back.

## Alternatives

- **Per-receiver-type post-typeck inference, keeping the return-type rule.**
  Fixes Failure 1 (shadowing) — resolution per type kills the census — but
  keeps Failure 2: `filter` is still self-returning by shape, so it still
  silently mutates while `map` no-ops, and every future builder API is still
  a landmine. Inference from signature *shape* cannot see intent; rejected.
- **Warn instead of error on the discarded result.** Weighed honestly:
  witchy has no warning channel in the compile path (errors or silence; the
  LSP has hints), so a warning would be a new mechanism whose entire purpose
  is to let the silent-discard class survive half-silenced. The error is
  break-don't-deprecate applied to semantics, with `let _ =` as the
  intentional-discard spelling. If real-world migration pain proves too high,
  the fallback is staging: land the resolver + declarations first, flip the
  discard diagnostic from note-in-`witchy check` to hard error one release
  later — but the end state is the error.
- **A distinct marker keyword** (`mutating fn`, Hylo's `inout`). Cleanest
  avoidance of the `var` overload; rejected on net: a fourth convention row
  that means "var, but for the statement form", two spellings of write-back
  to teach, and no additional checking power — the signature-shape split in
  §2 already makes every case unambiguous. Revisit only if the overload
  confuses real users.
- **Nil-returning `var` procedures for std mutators**
  (`pub fn push(var xs: List(a), x: a):` reassigning internally). Makes the
  statement form the *only* form — no rewrite needed at all — but deletes the
  expression form (`let ys = xs.push(1)`) that value-semantics style is built
  on, breaks every existing `xs = list.push(xs, x)` call site, and destroys
  the self-assign shape the uniqueness pass optimizes. Rejected.
- **Do nothing.** The two probed failures are exactly the silent-misbehavior
  class the project's prime directive forbids; the evaluation ranked this
  fix #1 by severity × leverage.

## Drawbacks

- **A breaking migration with judgment calls in it.** The `var`-receiver list
  (§5) contains borderlines (`center`? `merge`?); wherever the call goes,
  some program's statement form changes meaning or starts erroring. Mitigated
  by: every change is loud (errors, not silent flips), the differential suite
  pins the deltas, and the fmt/suite vehicle is the standing migration
  practice.
- **The discard error will annoy** code that calls value-returning functions
  for a side-effecting closure (`xs.map(f)` for `f`'s prints). `let _ =` is
  two tokens; `for` loops are the honest spelling; still, it is friction.
- **`var`'s two readings** must be taught. The conventions table gains a
  row-split rather than a row; the checker enforces the boundary; but "var
  demands a var argument, except mutators in expression form" is genuinely
  subtler than today's uniform (and uniformly wrong-for-mutators) rule.
- **Sequencing.** Full receiver-type resolution wants
  [RFC-0046](0046-typed-trait-dispatch.md); statement-form coverage on more
  receiver types wants [RFC-0050](0050-method-call-generalization.md). This
  RFC can land on today's receiver typing (the resolver already types the
  receivers RFC-0028's sugar fires on), but its guarantees are only as total
  as dispatch is — un-typeable receivers keep their statement form
  unrewritten *and now un-errored-on is not acceptable*, so an unresolvable
  statement-position method call becomes an error too (it already is one
  downstream today: "cannot resolve the method call").

## Prior art

- Hylo (`inout`) and Swift (`mutating func`): mutation-of-receiver is a
  *declared* property of the function in every mutable-value-semantics
  language that shipped; none infers it from return-type shape.
  ([external-refs/mutable-value-semantics-2022](../external-refs/mutable-value-semantics-2022/notes.md)).
- Rust: `fn push(&mut self, …)` — the declaration doubles as the call-site
  requirement, the same split §2 encodes without references.
- RFC-0028's own Drawbacks section flagged "the statement-vs-expression
  distinction is subtle" and the linker comment (:624) admits the type-blind
  compromise ("whose overload would need the receiver type we don't infer
  here"). This RFC pays that debt.

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9).

**Status-accuracy corrections.** The `proposed` status is stale: implementation
is in flight on branch `impl/rfc-0043d` (locked worktree, 5 commits — the
classifier, the both-backend ABI exemption, std migration to var receivers, and
the census deleted with write-back moved to traits.rs). Both motivating silent
failure modes were probed live on the shipped binary: an impl-shadowed push loses
writes; a filter statement mutates while a map statement no-ops.

**Required revisions.** None to the design — the strongest of the reviewed set:
declaration-based (var first param + self-typed return), a checkable boundary,
parity by construction. Update the status when the branch merges. One migration
consequence to note: unresolvable statement-position method calls become hard
errors, so RFC-0046's remaining dispatch gaps convert into new compile errors
during migration.

**Verdict.** Implement-now — already in flight; let it land. Priority: high.
