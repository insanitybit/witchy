---
rfc: 0087
title: "Uniform var write-back: one parameter convention for every call"
status: implemented
created: 2026-07-13
implemented: 2026-07-16
predecessors:
  - "0043 (declared mutation - replaced the method-name census with var declarations)"
  - "0064 (complete mutation classification - enforced the return-shape table this RFC removes)"
  - "0028 (ergonomic mutable value semantics - introduced statement-position place write-back)"
  - "0022 (place assignment - supplies the place read/write machinery generalized here)"
related:
  - "0046 (typed trait dispatch - conventions travel with the resolved declaration)"
  - "0050 (method-call generalization - method and free-call forms must agree)"
  - "0051 (memory-safety invariants - optimization follows ownership facts, never method names)"
  - "0070 (0.1 blocking set - one spelling per concept, break rather than deprecate)"
  - "0083 (opt-mode lifetimes - returned views must block write-back to their live owner)"
  - "0088 (ownership-aware extraction - optional no-copy implementation after semantics land)"
tracking: "Shipped with direct backend-parity conformance, compiler-resolved
  migration census, [RFC-0051](0051-memory-safety-invariants.md) seven-kernel performance evidence, and public
  guidance; closeout is recorded in [the RFC-0087 acceptance ledger](0087-acceptance-ledger.md)"
---

# RFC-0087: Uniform `var` write-back

> Provisional snippets; code blocks are deliberately not tagged `witchy` so
> the doc-example sweep does not execute pre-implementation code.

## Summary

Witchy already has the right declaration for mutable value flow: a `var`
parameter. This RFC makes that declaration mean one thing everywhere:

> A `var` parameter is move-in/move-out. Every call receives the argument's
> current value, gives the callee a mutable local, and writes that local's final
> value back to the caller's place on every structured return. The function's
> ordinary return value is independent of write-back.

There is no return-type classification and no statement-versus-expression
split. These are all the same convention:

```
fn push(var xs: List(a), value: a) -> Nil
fn pop(var xs: List(a)) -> Option(a)
fn next_below(var r: Rng, bound: Int) -> Int
fn exchange(var slot: a, replacement: a) -> a
```

Calls use ordinary syntax:

```
var stack = []
stack.push(job)

while let Some(job) = stack.pop():
    run(job)

let face = rng.next_below(6)
```

The receiver must be a mutable place. The call may appear wherever an
expression of its declared return type may appear. Witchy specifies expression
evaluation order so effects are deterministic rather than restricting useful
calls to a bespoke set of "root" positions.

A bare statement may discard a call's ordinary result when the resolved call
has at least one `var` write-back. The write-back is the statement's effect. A
non-`var`, non-`Nil` call still requires an explicit `let _ =` discard.

This RFC supersedes [RFC-0043](0043-declared-mutation-writeback.md)'s return-shape table and [RFC-0064](0064-complete-mutation-classification.md)'s row-3 error.
It retains their central lesson: mutation is declared, resolved per concrete
callee, and never inferred from a method name or whole-program census. The
declaration is simply `var`; its meaning no longer changes with return type or
statement context.

## Motivation

### The current rule gives `var` two meanings

Today a `var` parameter means different things depending on the function's
return type:

- A function returning `Nil` uses move-in/move-out on every call.
- A first-parameter `var` function returning that parameter's type is a
  "mutator receiver": expression calls are pure, while method calls used as
  statements write back through a typed AST rewrite.
- Every other `var` plus non-`Nil` return is rejected.

That table solved a real historical problem. Before RFC-0043, Witchy guessed
mutation from names and return shapes, causing one method to mutate while an
apparently equivalent method silently discarded its result. Declaring `var`
was the correct fix. Making the declaration change meaning according to the
return type was not necessary to that fix.

The result is a hybrid reading rule:

```
xs.push(1)                 // statement: mutates xs
let ys = xs.push(1)        // expression: does not mutate xs
bump(counter)              // any position: mutates counter, returns Nil
```

The call syntax is identical, but context and return shape decide whether the
receiver changes. Extending that table with a third "fused" return shape would
preserve the underlying bifurcation and make generic and alias-heavy signatures
even harder to classify.

This RFC removes the table. `var` always writes back. A non-`var` parameter
never writes back.

### State plus value is an ordinary function shape

Advancing state and producing a value is common:

- popping a collection element;
- advancing a parser, lexer, cursor, iterator, or deterministic generator;
- inserting into a map while returning the displaced value;
- removing from a map or set while reporting what was removed;
- exchanging a value while returning the old value;
- updating state while returning a status, count, or event.

Witchy currently forces these APIs to return `(value, state)` tuples or split
one conceptual operation into multiple calls. `std/prng` exposes the tuple tax
directly:

```
let (face, next) = prng.next_below(rng, 6)
rng = next
```

Under uniform `var` write-back:

```
let face = rng.next_below(6)
```

The shorter spelling does not introduce references or shared mutation. The
callee receives a value and returns two channels at the ABI boundary: its
declared result and the final value of each `var` parameter. The caller commits
those values back into exclusive places.

### The pure/mutating distinction belongs on parameters

Pure transformations use ordinary parameters:

```
fn map(xs: List(a), f: fn(a) -> b) -> List(b)
fn filter(xs: List(a), keep: fn(a) -> Bool) -> List(a)
fn take(xs: List(a), n: Int) -> List(a)
```

Operations that update a caller-owned value use `var`:

```
fn push(var xs: List(a), value: a) -> Nil
fn sort(var xs: List(a)) -> Nil
fn pop(var xs: List(a)) -> Option(a)
```

To derive a changed copy, copy the value into a new mutable binding and mutate
that binding:

```
var sorted = original
sorted.sort()
```

That spelling describes the operation honestly. With value semantics and CoW,
the copy is logical; the optimizer may reuse storage when uniqueness proves it
safe.

## Design

### 1. One parameter convention

For every direct, method, trait, closure, or indirect function call:

- An ordinary parameter receives a value. Reassigning its callee-local binding
  cannot affect the caller.
- A `var` parameter receives a value from a mutable caller place. Its
  callee-local binding is mutable. On every structured return, its final value
  is written back to the same caller place.
- An `own` parameter consumes its argument. It has no caller write-back slot.

The declared return type is orthogonal. A function may have zero, one, or many
`var` parameters and return `Nil`, the receiver type, an unrelated type, a type
alias, or a generic type that happens to instantiate to the receiver type.

The following are all legal:

```
fn clear(var xs: List(a)) -> Nil
fn pop(var xs: List(a)) -> Option(a)
fn exchange(var slot: a, replacement: a) -> a
fn divide(n: Int, var quotient: Int, var remainder: Int) -> Bool
fn transfer(var from: Account, var to: Account, amount: Int) -> Result(Receipt, Error)
```

There is no receiver-only carve-out. A non-first or second `var` parameter has
the same move-in/move-out meaning as the first.

### 2. Calls require mutable places

Every `var` argument must be a mutable place:

- a `var` local;
- a `var` parameter;
- a field or index place rooted in either of those.

An immutable binding, literal, constructor result, function result, or other
temporary is rejected:

```
let xs = [1, 2]
xs.pop()                   // error: `pop` writes back through `xs`; declare it `var`

make_list().pop()          // error: write-back has no caller place
```

`move place` is also rejected for a `var` argument. `move` destroys the caller
slot; `var` promises to write a final value back into a live slot.

A `var` parameter cannot have a default argument. Omitting it would leave no
place to receive write-back.

### 3. Calls are ordinary expressions

A call's declared result can be used anywhere an expression of that type can
be used:

```
let pair = (stack.pop(), stack.length())
consume(stack.pop())
let value = cursor.next() ?? cursor.default_value()
let text = "next=${rng.next_below(10)}"
```

These programs are deterministic because evaluation order is specified in
section 4. The first tuple element completes its write-back before the second
element is evaluated. A `??` fallback observes the state after its left operand
returns `None`. An interpolation hole completes before the next hole starts.

This RFC deliberately does not introduce "root positions." Effects inside
expressions already exist through capability calls. A general-purpose language
needs a general order rule, not a mutation-specific expression grammar.

The discarded-result rule distinguishes calls that already have a declared
write-back effect from calls whose only product is their value. A non-`Nil` call
with at least one resolved `var` argument may be used as a bare statement; its
ordinary result is discarded after all write-backs commit:

```
stack.pop()                // legal: pop and discard the Option result
let _ = stack.pop()        // also legal: makes the discard explicit
```

A non-`var`, non-`Nil` call used as a bare statement remains an error:

```
parse(text)                // error: result discarded; use `let _ = parse(text)`
```

This is not a second mutation rule. A `var` call writes back in every context;
statement position merely declines its independent result channel. A
`Nil`-returning operation such as `stack.push(job)` remains a normal statement.

### 4. Evaluation order

Witchy evaluates user expressions in deterministic source order:

1. A method receiver is evaluated before its explicit arguments.
2. Call arguments are evaluated left to right as written. Keyword arguments
   preserve written order even when mapped to a different declaration order.
3. Indexing evaluates the base, then the index.
4. Unary operators evaluate their operand first. Binary operators evaluate the
   left operand before the right. `&&`, `||`, and `??` retain short-circuiting.
5. Tuple and list elements, constructor fields, record fields, and interpolation
   holes evaluate left to right in source order. A comprehension evaluates its
   generators from left to right as nested loops, each iterable in its ordinary
   iteration order; filters and the element expression run in written order for
   each reached iteration.
6. `if` evaluates its condition before its selected branch. `match`, `if let`,
   and `while let` evaluate the scrutinee before pattern selection and the
   selected body.
7. Assignment evaluates and captures the destination's place coordinates,
   evaluates the right-hand side, then performs the store.

A `var` argument is part of that order. At its position in the argument list,
the implementation evaluates its place coordinates exactly once and reads its
move-in value. After all arguments are evaluated, the callee runs. On return,
all write-backs commit before the call expression yields its declared result to
the enclosing expression.

Reservation does not remove or mutate the caller's visible value while later
arguments are evaluated. A later same-root read observes the value as of that
later argument position. The callee has not run and no write-back has committed,
so the read is a snapshot of the same pre-call value; CoW preserves that snapshot
if the callee subsequently changes its local copy.

Evaluating a `var` argument reserves that place as a write-back destination
until the call commits. Later argument expressions may read it: the read
produces an ordinary value snapshot, and CoW preserves that snapshot if the
callee later updates the `var` value. A later argument may not perform another
write-back to an overlapping place while that reservation is live. A write-back
completed by an earlier argument is no longer reserved, so legality follows the
same source order as execution. The checker uses resolved call effects to reject
the live conflict locally:

```
push(stack, stack.last())                 // legal: later argument only reads
outer(stack, inner(stack))                // error if both calls write `stack`
outer(inner(stack), stack)                // legal: inner commits before reserve
```

The written argument order therefore matters in the same visible way it does
for every other effect. Keyword-argument temporary lowering preserves this
order before rearranging values into declaration order.

For example:

```
let pair = (queue.pop(), queue.length())
```

means pop, commit `queue`, then read its new length.

For assignment:

```
table[index()] = queue.pop() ?? default
```

means evaluate `table`'s destination path and `index()` once, evaluate the RHS
(including `queue` write-back), then store into the current value of `table` at
the captured path. Capturing a place records its root and projection
coordinates, not an old aggregate snapshot. If the RHS changes the same root
such that the captured projection is no longer valid, the final store fails by
the same bounds/field rule as an ordinary place assignment.

This is sequential access, not an overlapping-`var` reservation: the assignment
has not moved a value out or promised a later call write-back when its coordinates
are captured. The RHS completes before the assignment store. The checker does
not reject same-root RHS mutation merely because it may make an index invalid.

This section extends the source-order guarantee already required for keyword
arguments to every expression family. Both backends must share the same typed
sequencing lowering.

### 5. Exclusive write-back places

Move-in/move-out requires exclusive write access from evaluation of a `var`
argument through the call's write-back commit. Ordinary value reads remain
legal and create snapshots; another write-back in the same call expression and
two `var` arguments may not overlap:

```
transfer(account, account, 10)          // error: overlapping `var` places
swap(xs[i], xs[i])                      // error
```

For v1, two `var` places rooted in the same base variable are accepted only
when the checker can prove their projections disjoint. Otherwise the call is
rejected conservatively:

```
swap(xs[0], xs[1])                      // may be accepted: distinct constants
swap(xs[i], xs[j])                      // rejected unless inequality is proven
```

This is a call-scoped exclusivity check, not a general borrow or lifetime
model. It inspects only the receiver and argument expressions of one
synchronous call. No reference escapes and no alias is created. A future
opt-mode lifetime model can reuse the same place-conflict judgment without
changing source semantics.

Logically, write-backs commit together after a structured return. Because their
places are disjoint, commit order is unobservable. Implementations may perform
the stores in declaration order.

### 6. Structured return and failure

Write-back occurs on every structured completion of the callee:

- the function body tail;
- explicit `return`;
- `?` propagation inside the callee;
- return from a closure with `var` parameters.

The callee's write-backs commit before a caller-side `?`, `??`, pattern match,
or surrounding expression continues.

A runtime trap or host failure that aborts evaluation is not a structured
return and makes no partial-write-back guarantee. No user code resumes after an
uncaught trap. Host boundaries that catch failures must represent them as a
normal `Result` if write-back state is observable.

`?` is an ordinary early return, not a transaction boundary. Its desugared
`match` plus `return Err(e)`/`return None` form has exactly the same write-back
behavior as the `?` spelling. A function that needs rollback stages changes in
ordinary locals and assigns its `var` parameters only after every fallible step
succeeds.

Write-back is all-or-nothing at the language level: every final `var` value from
one structured return commits together, and the caller cannot observe a subset.
This is commit atomicity, not rollback of mutations merely because the ordinary
result is `Err` or `None`.

#### [RFC-0088](0088-ownership-aware-extraction.md) amendment disposition

Implementation feedback first recorded alongside RFC-0088 is resolved in this
RFC, so one semantic fact never has two normative homes:

- Section 4 owns captured assignment coordinates and current-root bounds
  behavior; an invalidated projection takes the ordinary assignment trap.
- This section owns structured completion; callee-side `?` commits exactly like
  its explicit-return desugaring.
- [RFC-0083](0083-opt-mode-lifetimes.md) owns borrowed-view lifetime and loan rules. [RFC-0088](0088-ownership-aware-extraction.md) may consume
  those facts for optimization but cannot extend their source semantics.

RFC-0088 is consequently an optimization RFC only. Future view-lifetime or
operation-specific optimization work belongs to its actual owning RFC and does
not amend uniform write-back implicitly.

### 7. Methods, traits, and free calls agree

Method syntax is call sugar only:

```
let x = stack.pop()
let x = list.pop(stack)
```

Both forms resolve to the same declaration, require the same mutable place,
perform the same write-back, and return the same value. There is no
statement-only method rewrite and no pure free-call escape.

An impl or trait method may declare `var self` with any return type:

```
trait Cursor(a):
    fn next(var self) -> Option(a)
```

An implementation must match parameter conventions exactly. Trait dispatch
resolves the concrete declaration through the typed method table, then uses the
conventions already present in the trait signature. Convention mismatch is a
type error, not an adapter.

### 8. Function values carry conventions

Parameter conventions are part of function types:

```
fn(List(Int)) -> Option(Int)
fn(var List(Int)) -> Option(Int)
fn(own Bytes) -> Digest
```

These types are distinct and do not implicitly coerce. A function with a `var`
parameter can be passed, returned, stored, and called indirectly; the indirect
call still requires a mutable place and uses the same move-in/move-out ABI.

Closures may declare `var` parameters:

```
let take = fn(var xs: List(Int)) -> Option(Int):
    xs.pop()
```

The closure's parameter is local to each call. Captured variables remain by
value. A closure cannot use `var` to mutate a captured outer binding; it must
receive that binding as a `var` argument. This preserves deterministic capture
semantics.

Eta expansion, method references, reflection, generated code, and comptime
`TypeInfo` must retain conventions. A linker may not turn a `var` function into
an ordinary-parameter lambda.

### 9. Async and generator boundaries

A synchronous call with `var` arguments has one completion point at which
write-back commits. An `async fn` or `gen fn` suspends and therefore cannot take
`var` parameters in v1: keeping a caller place live across suspension would
require a lifetime and exclusivity model beyond this RFC.

Ordinary mutable locals inside async and generator bodies may be passed to
synchronous `var` calls before or after `await`, or between `yield`s. Their
write-back completes before the next suspension point.

The shipped async lowering threads locals live across `await` as parameters of
ordinary segment functions. A synchronous `var` call on such a local must write
back before the segment constructs its continuation. This seam is a required
differential test; it does not depend on the deferred frame-record design from
RFC-0059.

A later opt-mode lifetime RFC may admit asynchronous `var` parameters by
proving that a place remains exclusively live until task completion. That is a
compatible extension; this RFC establishes the synchronous semantics it must
preserve.

### 10. Typed lowering and ABI

Resolution annotates every call with its concrete parameter conventions before
either backend executes it. The shared typed representation records, for each
`var` argument, a place plan rather than a plain value expression:

```
Call {
    callee,
    arguments,
    conventions,
    writeback_places,
    result_type,
}
```

A place plan contains the mutable root plus field/index projections. Its
computed indices are evaluated once into temporaries. The same plan supplies
the move-in read and the move-out store.

At the ABI boundary a function returns:

```
(declared_result, final_var_0, final_var_1, ...)
```

The compiled backend already has the basic multi-result vehicle in
`CallStoreMulti`. Every returned `var` value is first captured in a scratch
local; the typed lowerer then commits that scratch through its place plan. A
plain local destination may be written directly as an optimization. Indirect
calls require the same result envelope. Early-return lowering appends every
current `var` parameter to the result tuple.

The interpreter must use the same call outcome rather than special-casing
simple `Expr::Var` arguments in its environment:

```
CallOutcome {
    value,
    writebacks,
}
```

Both backends consume the same resolved convention and place plan. No backend
may infer write-back from a function name, return shape, statement position, or
runtime value.

This place-plan machinery is a prerequisite, not incidental plumbing. The
current interpreter accepts only a bare variable as a procedure-style write-back
destination, and the compiled fast path likewise assumes local destinations.
Implementation therefore proceeds oracle-first: build and adversarially test the
interpreter's nested read/store plan, then make WIR lowering consume the same
resolved plan. RFC acceptance is blocked until nested field/index plans,
single-evaluation coordinates, alias snapshots, and post-RHS bounds failures
agree on both backends.

### 11. Ownership and optimization

Uniform `var` is a semantic ownership convention, not an optimization hint:

- move-in gives the callee the caller place's current value;
- move-out returns the final value;
- exclusivity prevents overlapping writes;
- value semantics remain observable even when storage is shared.

The uniqueness pass may optimize an unaliased `var` value in place. When the
value is shared it must preserve CoW behavior. Per the standing RFC-0051 rule,
new operations may not add method-name-specific `*_cap` helpers or `self_*`
recognizers.

The semantic cut must preserve the existing load-bearing in-place family in the
same Phase-1 change that removes statement-mutator reassignment. Today those
paths recognize `x = f(x, ...)`; after this RFC they must be re-keyed to the
resolved typed `var` call plus its write-back place. Existing `list.push`,
`list.set_at`/`update_at`, `dict.insert`/`update`, and string-concat helpers remain
until a separately measured general mechanism replaces them. Landing correct
write-back while silently dropping those paths is not acceptable: RFC-0051
measured OOM and multi-fold regressions without them.

The write-back ABI alone does not make `List.pop` O(1). A correct baseline may
copy. An optimized pop needs a general ownership-aware extraction operation in
WIR, parameterized by container layout and projection, that can serve list
extraction, dictionary removal, and future iterator advancement:

- unique container: move the selected element out and update metadata in place;
- shared container: preserve the original, construct the updated value, and
  retain the returned element correctly;
- invalid projection: return the operation's declared `None`/`Err` without
  corrupting either channel.

No complexity or no-copy claim is part of the public contract until heap,
aliasing, and benchmark tests prove that general path. Semantics land first;
optimization must be independently measurable and memory-safe.

## Standard-library migration

The repository migrates in one breaking cut. There are no deprecated twins.

### Collections

Imperative collection operations use `var` and return their natural auxiliary
result or `Nil`:

```
List.push(var self, value) -> Nil
List.pop(var self) -> Option(a)
List.pop_front(var self) -> Option(a)
List.sort(var self) -> Nil
List.reverse(var self) -> Nil
List.set_at(var self, index, value) -> Nil
List.swap(var self, i: Int, j: Int) -> Nil

Dict.insert(var self, key, value) -> Option(v)
Dict.remove(var self, key) -> Option(v)
Set.insert(var self, value) -> Bool
Set.remove(var self, value) -> Bool
```

The exact `Dict`/`Set` auxiliary result follows their totality contract, but no
self-returning mutator twin remains. Their common statement form may discard the
auxiliary result without `let _ =` because the resolved call has `var`
write-back. Pure queries and transformations retain ordinary parameters:

```
List.map(self, f) -> List(b)
List.filter(self, keep) -> List(a)
List.take(self, n) -> List(a)
Dict.get(self, key) -> Option(v)
```

This removes the former "frozen family" problem: an existing mutator can return
a displaced or removed value without changing its write-back classification,
because return type never classified it.

### Deterministic randomness

`std/prng` becomes:

```
Rng.next(var self) -> Int
Rng.next_below(var self, bound: Int) -> Int
Rng.next_bool(var self) -> Bool
Rng.choice(var self, xs: List(a)) -> Option(a)
```

The tuple forms are deleted in the same cut. `std/rand` keeps its existing
capability-backed value-returning surface; its state remains host-owned.

The dice example becomes:

```
var rng = prng.seed(7)
var rolls = []
for _ in 0..10:
    rolls.push(rng.next_below(6) + 1)
```

This call in argument position is intentional and deterministic under section
4. No mutation-specific expression restriction is required.

Dictionary construction that formerly chained through temporary receivers uses
the existing total constructor:

```
let ages = dict.from_pairs([("ada", 36), ("bob", 41)])
```

`dict.new().insert(...)` is intentionally not retained: a temporary has no
caller place for write-back.

### API audit rule

Every current self-returning `var` function is audited:

- If the operation is an imperative update, keep `var` and change its return to
  `Nil` or a useful auxiliary result.
- If the operation is a pure transformation, remove `var` and keep the returned
  value.
- Do not ship both a mutating and pure spelling of the same concept merely to
  preserve old chaining.

Names should make the ordinary API distinction legible (`map`/`filter` as
transformations; `push`/`pop`/`insert` as updates), but names never control
semantics. The signature does.

## Source migration

Common rewrites are mechanical:

```
xs = xs.push(value)
```

becomes:

```
xs.push(value)
```

A pure derived copy:

```
let ys = xs.push(value)
```

becomes:

```
var ys = xs
ys.push(value)
```

Tuple-threaded state:

```
let (value, next) = step(state)
state = next
```

becomes:

```
let value = step(state)
```

Before flipping semantics, the implementation branch runs a type-resolved
mutation census over std, examples, book, and projects. It classifies every
resolved current mutator call as a mechanical self-rebind, statement mutation,
derived-copy expression, nested/argument call, or temporary-receiver chain, and
classifies every `var self -> Self` declaration as imperative or pure. A regex
count is not acceptance evidence.

Temporary migration tooling makes expression-position calls to an old resolved
mutator receiver loud during the repository cut, forcing an explicit
copy-first-or-mutate decision. This is migration tooling, not permanent source
semantics, and is removed before release. The formatter may perform unambiguous
statement rewrites, but it must not guess whether an expression call intended
mutation or a derived copy.

The review census at master `1d96bcfe` is a sizing floor, not the final report:
386 self-reassignment sites are expected to be mechanical; roughly 20
argument-position mutator calls require restructuring; the executed book example
contains one temporary-receiver insertion chain; 17 statement insertions exercise
the auxiliary-result discard decision; and 26 `var`-receiver operations require
signature classification. The compiler-produced, type-resolved report remains
authoritative because these classes overlap and source text alone cannot resolve
the callee.

Because Witchy is pre-0.1, this RFC chooses one coherent semantic cut rather
than editions, compatibility shims, or duplicate APIs. The repository, book,
examples, and projects migrate in the same change set.

## Diagnostics

Required diagnostics include:

- immutable or temporary `var` argument: name the callee, parameter, and the
  binding that must become `var`;
- overlapping `var` places: print both argument positions and their common
  mutable root; when a nested call conflicts with an earlier reservation, name
  the earlier argument and explain that written evaluation order keeps that
  reservation live;
- discarded non-`Nil` result from a call without `var` write-back: preserve the
  existing `let _ =` teaching fix;
- `move` passed to `var`: explain that write-back needs a live place;
- defaulted `var` parameter: explain that omission has no write-back target;
- async/gen `var` parameter: name suspension and the deferred lifetime model;
- function-convention mismatch: render `fn(var T) -> U` versus `fn(T) -> U`;
- trait implementation mismatch: point to both the trait declaration and impl.

Diagnostics must use the resolved declaration. A same-named pure method on one
type and `var` method on another must never interfere.

## Acceptance criteria

The RFC is implemented only when all of the following are true:

1. Declaration checking accepts every return type and parameter position for
   `var`; the RFC-0064 row-3 error is removed.
2. Every direct and method call writes back every `var` parameter on tail,
   explicit `return`, and callee-side `?`; a partial-progress multi-`var` test
   proves that `?`, its explicit-return desugaring, and tail `Err` commit the same
   final values. The matrix includes a Result/Option receiver mutator with an
   additional `var` parameter so mutator classification cannot bypass the `?`
   path.
3. Immutable places, temporaries, `move`, defaults, and overlapping places fail
   with the diagnostics above.
4. Nested field/index places evaluate each index exactly once and write back on
   both backends.
5. Expression-order tests cover calls, operators, tuples/lists,
   comprehensions, interpolation, `??`, match/if, and assignment with observable
   effects.
6. Free-call and method-call forms have identical behavior.
7. Trait declarations and implementations preserve conventions through typed
   dispatch.
8. Function values and closures carry conventions through direct and indirect
   calls on both backends.
9. Async/gen parameters reject while synchronous `var` calls on their locals
   work across suspension points, including a local threaded through the shipped
   async segment-function lowering.
10. A bare call with resolved `var` write-back may discard its ordinary result;
    a non-`var`, non-`Nil` bare call still rejects; `let _ =` remains an explicit
    discard in either case.
11. Differential tests cover zero, one, and multiple `var` parameters; generic
    and alias-equal return types; same-typed auxiliary results; and caller-side
    `?`/`??` after write-back.
12. The stdlib, examples, book, and projects contain no old self-returning
    mutator convention or tuple-threaded PRNG call.
13. `spec/language.md` states the convention, exclusivity rule, function-type
    syntax, and complete evaluation order. `spec/stdlib.md` is regenerated.
14. Any optimized extraction path has aliasing, refcount, heap-bound, and
    benchmark evidence; otherwise no O(1) or no-copy claim is published.
15. The Phase-1 semantic cut preserves RFC-0051's existing in-place behavior.
    `word_count`, `dict_count`, `list_sum`, and `knucleotide` do not OOM, and
    `list_index`, `binary_trees`, and `expr_eval` stay within RFC-0051's recorded
    non-regression thresholds. `WITCHY_OPT=-inplace` remains the forced-copy
    differential oracle, not the release configuration.
16. A checked-in type-resolved migration report accounts for every affected
    declaration and call in std, examples, book, and projects before the old
    rewrite is deleted.
17. This cut lands before 0.1 as RFC-0070's uniform-mutation coherence item; the
    release ledger and changelog do not describe the superseded two-shape rule.

## Alternatives

### Return-shape-classified fused mutators

Rejected. Classification by "return type differs from receiver" is unstable
under aliases and generic instantiation. It also preserves the existing hybrid
where an identical call mutates in one context and is pure in another. The
return type should describe the result, not secretly select the parameter ABI.

### A `mutating` declaration keyword

Rejected. `var` already declares mutable move-in/move-out data flow. A second
keyword would create two declarations for the same fact and invite disagreement
between them.

### A call-site sigil such as `pop!()`

Rejected. It would bifurcate ordinary calls into a second syntax family and
still require the signature to determine which places write back. Witchy puts
authority and ownership facts in types and parameter conventions; it does not
repeat every effect in call punctuation.

### Root-position-only calls

Rejected. Recursive root definitions become an ad hoc effect system and still
permit observable sequencing through `??`, branch bodies, and assignment
places. Pinning general expression order is simpler, more powerful, and useful
for capability effects as well as value write-back.

### Preserve the self-returning pure expression form

Rejected. Keeping `let ys = xs.push(1)` pure while making `let x = xs.pop()`
mutate is the exact return-shape bifurcation this RFC removes. A changed copy is
spelled as a copied binding followed by an ordinary mutation.

### Tuple threading only

Rejected as the only spelling. Tuples remain available when both outputs are
ordinary values, but forcing callers to manually thread a uniquely owned state
duplicates the move-in/move-out mechanism Witchy already has.

### References or shared mutable cells

Rejected. They solve a larger problem by introducing aliasing and lifetime
obligations. Uniform `var` keeps mutation scoped to a call and writes values
back into exclusive caller places.

## Drawbacks

- A reader may need the resolved signature to know that a call writes back.
  This is already true for `var` procedures, capability effects, and dynamic
  trait dispatch. Editors and generated docs must render conventions clearly.
- Existing self-returning mutator expression calls change semantics and must be
  migrated. This is a deliberate pre-0.1 break, not a silent compatibility
  shim.
- Pure chaining through imperative names disappears. Deriving a changed copy
  takes a mutable local, which is longer but honest about the copy and update.
- Bare statement calls with `var` write-back may discard an auxiliary result.
  This trades the blanket unused-result error for ergonomic imperative updates;
  APIs whose result must be inspected need an explicit future must-use contract,
  not an accidental dependence on whether they mutate.
- Specifying full expression order constrains future optimizer reordering.
  Observable effects already require that constraint; optimizers may reorder
  only when effect and alias analysis proves equivalence.
- Convention-bearing function values require an indirect multi-result ABI and
  expand the type surface. Rejecting them would leave a direct/indirect semantic
  split, so the implementation cost is part of the feature.
- Conservative exclusivity rejects some dynamically disjoint places. A future
  place/lifetime analysis may accept more programs without changing behavior.
- The standard-library cut is broad. It touches mutation-heavy examples and
  projects at once. The result is one rule rather than a permanent compatibility
  layer.

## Prior art

- Swift's `inout` parameters use copy-in/copy-out semantics and compose with
  ordinary return values. Swift also enforces exclusive access to overlapping
  mutable places.
- Hylo models mutable value flow through `inout` projections without making
  shared references the default.
- Rust methods routinely combine `&mut self` with auxiliary returns. Rust pays
  for general references with a lifetime system; Witchy keeps the write-back
  confined to a synchronous call.
- Go evaluates calls and assignments in a specified order and uses ordinary
  call syntax for methods that mutate through pointer receivers. Witchy's
  `var` convention provides value-style move-in/move-out instead of pointers.
- Ruby uses ordinary call syntax for stateful methods. Its `!` suffix is a
  naming convention rather than an enforced effect boundary; Witchy keeps the
  enforceable fact in the signature.
- Elixir's `{value, state}` APIs demonstrate that tuple threading scales, but
  also demonstrate the ceremony this RFC removes where the language already
  has an exclusive write-back channel.

## Amendments to earlier RFCs

On acceptance:

- RFC-0043's name-census removal and declaration-first resolution remain in
  force. Its return-shape classification and statement-only mutator rewrite are
  superseded.
- RFC-0064's declaration check rejecting non-`Nil`, non-self `var` functions is
  deleted. Its discarded-result error remains for non-`var` calls; a call with a
  resolved `var` write-back may be a statement because write-back is already an
  observable declared effect.
- RFC-0028's place machinery remains, but method-statement write-back is no
  longer a special desugar. Typed `var` calls use generalized place plans.
- RFC-0050 method syntax continues to be ordinary resolved call sugar; this RFC
  requires method and free forms to share conventions and write-back behavior.
- RFC-0051's ownership and memory-safety invariants remain unchanged. `var`
  supplies a general ownership fact; no method-specific optimization is added.

> 2026-07-16: **IMPLEMENTATION CLOSEOUT.** The dedicated current-master matrix
> proves write-back, ordering, exclusivity, structured return (including
> callee-side `?`), traps, function-value identity, comprehensions, closures,
> generators, and the shipped async segment seam on the interpreter and
> compiled Wasm backends. Captured assignment projections now apply to the
> post-RHS current root with ordinary bounds behavior. Exact negative
> diagnostics remain source-facing. The compiler-resolved repository census,
> seven-kernel RFC-0051 performance gate, spec/book guidance, and RFC-0088
> amendment disposition are current and green; see
> `rfcs/0087-acceptance-ledger.md`.
