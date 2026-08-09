# Appendix: Performance - the Ownership Knobs

The parameter conventions aren't performance annotations, and it's worth saying
so before this appendix tempts you to treat them as such. `let`/`var`/`own`
from [the functions chapter](tour-functions.md) exist to preserve value
semantics. What they *also* do is hand the compiler facts it can act on without
a garbage collector.

This appendix separates the source-level meaning from the optimizations it
enables.

First, the ground rule: witchy has value semantics. A callee must never be
able to mutate what the caller still observes. That guarantee is exactly what
makes witchy's aggressive optimizations *sound*: when the compiler can prove
a value has one owner, it's free to mutate it in place, because no one else
can be looking.

| You write | What it means | What the optimizer gets |
|---|---|---|
| `fn f(xs: List(Int))` *(default)* | an owned, observably immutable value | the callee's copy is independent; safe everywhere |
| `fn f(let xs: List(Int))` | an immutable **borrow** - the type checker rejects returning it, so it can't outlive the call | the value provably has no new owner after the call; backends share it without a defensive copy |
| `fn f(own xs: List(Int))` | ownership transfer; use-after-move is a compile error | the callee may consume the value in place |
| `fn f(var n: Int)` | the callee mutates and the caller's `var` is written back | mutate-in-place with write-back; no copy-out |

## The optimizations you get without asking

Most of the compiled tier's speed comes from machinery that needs **no
annotations** - it triggers on shapes the compiler can prove unaliased:

- **Linear update, analysis-driven.** A uniform `var` accumulation -
  `xs.push(e)`, `s = s + piece`, `d.insert(k, v)`,
  `d.update(k, dflt, f)` - may mutate the collection in place with
  capacity doubling. The *uniqueness pass* decides when that's sound: an
  alias zeroes the ownership token exactly where it's created (one copy
  re-owns; it doesn't disqualify the whole function), a **read-only helper
  call in the loop doesn't break the chain** (function summaries prove its
  parameter never escapes), and a `let`-annotated parameter is certified by
  the type checker. Value semantics never bend - an aliased buffer is
  copied, and `witchy check` tells you where and why ("`ys` is rebuilt by
  copy on every iteration…"). Both backends apply this.
- **`own`/`move` pipelines.** A function with one `own` collection parameter
  that returns it carries the ownership token *across* the call:

  ```witchy
  fn grow(own xs: List(Int), n: Int) -> List(Int):
      var out = move xs
      out.push(n)
      out

  // 100k iterations, one owned buffer end to end.
  // O(n), ~8 ms compiled.
  xs = grow(move xs, i)
  ```

  This is what the ownership annotations buy at runtime: the signature
  proves the transfer, so the compiler threads the buffer's spare capacity
  through the call instead of copying at every boundary.
- **Threads through your own types, too.** None of this is limited to the
  builtin collections. A record field update `s.count = s.count + 1` mutates
  the record in place; growing a field's list `s.items.push(x)`
  grows *that* buffer in place; and an `own` record parameter threaded through a
  function (`s = bump(move s)`) keeps the record uniquely owned across the call.
  So a wrapper type carries the same zero-copy behavior as the collection it
  wraps - a `Stack`'s push is as cheap as a raw `list.push`, with no annotation
  beyond `own` on a threaded parameter. (The same uniqueness pass drives it; an
  aliased field, like an aliased variable, falls back to a copy.)
- **Functional-in-place state kernels.** In `mode opt`, direct self recursion
  over one `own unique` scalar-field record can carry a stronger guarantee:
  update only that record's fields, use scalar auxiliary parameters, explicitly
  return the owner on every base path, and pass it directly
  to the recursive call as the function's final expression. The compiler
  forwards both the value and its hidden
  ownership token through one loop. Recursive depth then adds no allocation,
  free, arena rewind, or stack growth. There's no `fip` keyword; violating the
  shape is a source-located opt-mode error. `witchy stats` exposes the allocator
  and reclaimer call counts used to verify the guarantee.
- **Update and extract.** `xs.pop()`, `d.insert(k, v)`, and `d.remove(k)` carry
    the collection token through the general `var` ABI while returning the old
    leaf independently. Direct calls, typed function values, typed lambdas, and
    existential trait witnesses transport the same result, write-back value,
    and ownership state. A unique `pop` moves the leaf without a spine copy;
    dictionary insert/remove perform one semantic lookup. Shared roots use
    copy-on-write in normal mode. Their receivers are declared `unique`, so
    `mode opt` rejects an aliased or actively loaned owner instead of taking the
    copy; the diagnostic points to the ownership-loss reason. The `witchy stats`
    extraction counters make searches, copied bytes, retains, and drops directly
    inspectable, while `indirect_ownership_calls` identifies state-bearing calls
    that retained typed table dispatch.
- **Dict hash index.** Dicts carry a hidden open-addressing index; lookups,
  `has`, `get_or`, and upserts are O(1) while iteration order stays
  insertion order.
- **Loop watermark resets.** A loop body whose allocations provably don't
  escape the iteration rewinds the arena every pass - a million-iteration
  formatting loop runs in constant memory.

The honest summary of where the knobs matter: `let`/`own`/`var` are
*contracts* the optimizer consumes - `let` certifies a call as
chain-preserving, `own`+`move` threads ownership through call boundaries,
and everything unannotated is still analyzed (summaries are computed for
every function). Write the default first; reach for annotations when they
say something true about ownership, and for `region:` when a profile (or a
`witchy check` cliff note) says so.

## Regions: scoping your allocations

`region:` is the allocation-lifetime knob. Everything allocated inside the
block is reclaimed when it ends; the block's value is what survives - and
only its region-born bytes are copied out (anything from outside the region
is shared, verifiably: run with `WITCHY_REGION_STATS=1` and a parent-side
passthrough reports zero bytes copied). Use it around parse-then-summarize
shapes, per-item work in long loops the automatic reset can't cover, and
anywhere a burst of temporaries would otherwise live until the enclosing
boundary:

```witchy
fn summarize_text(text: String) -> String:
    region -> String:
        let parsed = text + "!"
        parsed

fn main(console: Console):
    console.print(summarize_text("processed"))
```

```text
processed!
```

The compiler already inserts the same machinery for free where it can prove
it safe (escape-free loop iterations); `region:` is the
explicit form for everything else.

## `packed` types: flat, cache-dense layouts

By default a `List(Point)` is an array of pointers to separately-boxed records.
For a fixed-scalar record scanned in a tight loop, declaring the type `packed`
stores the whole list as ONE flat inline buffer - `[count][x0, y0, x1, y1, …]` -
so a pass touches contiguous memory instead of chasing a pointer per element:

```witchy
import list

type Point packed:
    x: Int
    y: Int

fn main(console: Console):
    let ps = [Point(1, 2), Point(3, 4), Point(5, 6)]
    var total = 0
    var i = 0
    while i < ps.length():
        total = total + ps.at(i).x + ps.at(i).y
        i = i + 1
    console.print("${total}")
```

`packed` is a layout *contract*. Its inline fields must have closed fixed-size
layouts: scalars (`Int`/`Float`/`Bool`/`Duration`), nested packed records or
tuples, or fixed-layout packed tags. The `unbox` optimization, on by default in
release builds, assigns each such shape one canonical descriptor. Packed records,
lists, packed-containing tuples, and fixed-layout sums keep that descriptor across
direct named calls and calls linked between user modules; packed lists may also be
traversed with `for` and updated by the supported list operations.

The compiler rejects a boundary that has no exact packed ABI instead of silently
boxing the value. Direct generic calls specialize by their concrete logical
types, access envelope, parameter/result layout IDs, and optimization schema;
packed construction, indexed traversal, mutation, and return stay on that exact
physical instance. Function values and closures, trait/existential calls, host
calls, `region:` results, worker/channel transport, and rendering remain outside
the shipped matrix. Fixed-layout packed sums with derived/default structural
equality are the current whole-value equality exception: `==` and `!=` read the
descriptor's tag width and variant payload offsets. Custom `PartialEq` and other
specialized whole-value equality remain fail-closed.

Destination reuse and header removal are narrower optimizations over the same
descriptor. A compatible dead destination may be forwarded to a
constructor-complete direct producer of a `unique` packed record; fixed sums
support a proven nonescaping scratch. An RC header is removed only from a nonempty
immutable local packed list whose complete module use proves that it never
crosses, aliases, mutates, nests, or participates in a loan. The compiler retains
allocation or the RC header whenever those proofs don't hold.

## Borrowed views (`mode opt`)

A `mode opt` module may return a read-only **view** of a value it was given,
instead of copying it. The parameter names a lifetime with `let('a) T` and the
result borrows it with `View(T, 'a)`; the value is the same one at runtime (a
view has no representation of its own), so this changes only *when* a copy is
made, never the observable result:

```witchy
mode opt

fn first(let text: let('a) String) -> View(String, 'a):
    text

fn main(console: Console):
    var s = "borrowed, not copied"
    let view = first(s)
    console.print(view)
    s = "now the view is done, so the owner is free again"
    console.print(s)
```

While a view is live, its owner is *loaned*: you may not move, reassign, or
mutate the owner, pass it to a `var`/`own` parameter, or let the view escape
through a closure, task, or channel - the checker rejects each with a diagnostic
that names the owner, the borrowing call, and the fix. The loan ends at the
view's last use (as above, `s` is free again on the next line) or when you
materialize an owned copy with `view.owned()` (from `import borrow`).
Materializing copies out, so the owner is free immediately even though the owned
value lives on:

```witchy
mode opt

import borrow

fn first(let text: let('a) String) -> View(String, 'a):
    text

fn main(console: Console):
    var s = "borrowed"
    let kept = first(s).owned()
    s = "the view is materialized, so the owner is free right away"
    console.print(kept)
    console.print(s)
```

`.owned()` is a blanket-impl trait method (`std/borrow`), so it dispatches through
the ordinary typed method path - on a view it copies out, and on a non-view value
it's a plain identity. Views are a `mode opt`-only tool: normal witchy keeps
owned value semantics and never needs the syntax.

The lifetime relation is part of a function value's type, not a property of its
spelling. Calling through `let inspect = first` preserves the same loan, and an
ascription that removes the returned-view relation is rejected. On the compiled
backend, a hidden owner root remains live through the view's last use (including
explicit `return` and `?` paths), and the active loan makes the owner non-unique
to update/extract optimizations. That is why materializing and then mutating the
owner preserves the old snapshot instead of modifying shared storage in place.

A persisted view must come from a stable owner. A view of a temporary is useful
only when immediately materialized with `.owned()`, and a borrowed result can't
be stored in a mutable binding or owned aggregate. Forwarding a view keeps the
original owner loan, including through lambdas and indirect calls. A live view
also can't cross an `await` or a loop `break`/`continue` edge; materialize it
first when ownership must outlive that boundary.

Last-use checking is precise within a straight-line block. An enclosing live
view is conservatively treated as live throughout a nested branch or loop body,
so materialize before branch-local mutation when the branches can't be proven
disjoint.

### Borrowed nominal shells

A lifetime-parameterized nominal can hold a view together with owned cursor
state. The compiled backend retains the owner root, not the view address, and
releases it at the shell's checked last use. Updating an owned scalar field
keeps the same root; replacing a declared borrowed field retires the old root
after the write-back reads it and then retains the replacement root.

`List(B('a))` is supported for a direct borrowed nominal `B`: list literals,
`list.at`, and `for` traversal preserve hidden owner companions without copying
the viewed payload. A dynamic `list.at` deliberately retains every possible
element owner, which is conservative and correct. Mutating such a list or
passing it through a relation-erasing boundary remains unavailable until the
compiler has per-element overwrite/drop descriptors.
