# Appendix: Performance — the Ownership Knobs

The parameter conventions from [the functions chapter](tour-functions.md) are
not just a correctness model — they are what lets the compiler optimize
without a garbage collector. This appendix says what each one means to the
optimizer and which knobs actually move the needle.

First, the ground rule: witchy has value semantics. A callee must never be
able to mutate what the caller still observes. That guarantee is exactly what
makes witchy's aggressive optimizations *sound*: when the compiler can prove
a value has one owner, it is free to mutate it in place, because no one else
can be looking.

| You write | What it means | What the optimizer gets |
|---|---|---|
| `fn f(xs: List(Int))` *(default)* | an owned, observably immutable value | the callee's copy is independent; safe everywhere |
| `fn f(let xs: List(Int))` | an immutable **borrow** — the type checker rejects returning it, so it cannot outlive the call | the value provably has no new owner after the call; backends share it without a defensive copy |
| `fn f(own xs: List(Int))` | ownership transfer; use-after-move is a compile error | the callee may consume the value in place — its story ends here |
| `fn f(var n: Int)` | the callee mutates and the caller's `var` is written back | mutate-in-place with write-back; no copy-out |

## The optimizations you get without asking

Most of the compiled tier's speed comes from machinery that needs **no
annotations** — it triggers on shapes the compiler can prove unaliased:

- **Linear update, analysis-driven.** A self-assign accumulation —
  `xs = list.push(xs, e)`, `s = s + piece`, `d = dict.insert(d, k, v)`,
  `d = dict.update(d, k, dflt, f)` — mutates the collection in place with
  capacity doubling. The *uniqueness pass* decides when that is sound: an
  alias zeroes the ownership token exactly where it is created (one copy
  re-owns; it does not disqualify the whole function), a **read-only helper
  call in the loop doesn't break the chain** (function summaries prove its
  parameter never escapes), and a `let`-annotated parameter is certified by
  the type checker. Value semantics never bend — an aliased buffer is
  copied, and `witchy check` tells you where and why ("`ys` is rebuilt by
  copy on every iteration…"). Both backends apply this.
- **`own`/`move` pipelines.** A function with one `own` collection parameter
  that returns it carries the ownership token *across* the call:

  ```witchy
  fn grow(own xs: List(Int), n: Int) -> List(Int):
      xs = xs.push(n)
      xs

  // 100k iterations, one owned buffer end to end — O(n), ~8 ms compiled.
  xs = grow(move xs, i)
  ```

  This is what the ownership annotations buy at runtime: the signature
  proves the transfer, so the compiler threads the buffer's spare capacity
  through the call instead of copying at every boundary.
- **Threads through your own types, too.** None of this is limited to the
  builtin collections. A record field update `s.count = s.count + 1` mutates
  the record in place; growing a field's list `s.items = list.push(s.items, x)`
  grows *that* buffer in place; and an `own` record parameter threaded through a
  function (`s = bump(move s)`) keeps the record uniquely owned across the call.
  So a wrapper type carries the same zero-copy behavior as the collection it
  wraps — a `Stack`'s push is as cheap as a raw `list.push`, with no annotation
  beyond `own` on a threaded parameter. (The same uniqueness pass drives it; an
  aliased field, like an aliased variable, falls back to a copy.)
- **Dict hash index.** Dicts carry a hidden open-addressing index; lookups,
  `has`, `get_or`, and upserts are O(1) while iteration order stays
  insertion order.
- **Loop watermark resets.** A loop body whose allocations provably don't
  escape the iteration rewinds the arena every pass — a million-iteration
  formatting loop runs in constant memory.

The honest summary of where the knobs matter: `let`/`own`/`var` are
*contracts* the optimizer consumes — `let` certifies a call as
chain-preserving, `own`+`move` threads ownership through call boundaries,
and everything unannotated is still analyzed (summaries are computed for
every function). Write the default first; reach for annotations when they
say something true about ownership, and for `region:` when a profile (or a
`witchy check` cliff note) says so.

## Regions: scoping your allocations

`region:` is the allocation-lifetime knob. Everything allocated inside the
block is reclaimed when it ends; the block's value is what survives — and
only its region-born bytes are copied out (anything from outside the region
is shared, verifiably: run with `WITCHY_REGION_STATS=1` and a parent-side
passthrough reports zero bytes copied). Use it around parse-then-summarize
shapes, per-item work in long loops the automatic reset can't cover, and
anywhere a burst of temporaries would otherwise live until the enclosing
boundary:

```witchy
let summary = region -> String:
    let parsed = parse_huge_input(text)
    summarize(parsed)
```

The compiler already inserts the same machinery for free where it can prove
it safe (escape-free loop iterations); `region:` is the
explicit form for everything else.

## `packed` types: flat, cache-dense layouts

By default a `List(Point)` is an array of pointers to separately-boxed records.
For a fixed-scalar record scanned in a tight loop, declaring the type `packed`
stores the whole list as ONE flat inline buffer — `[count][x0, y0, x1, y1, …]` —
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
    while i < list.length(ps):
        total = total + list.at(ps, i).x + list.at(ps, i).y
        i = i + 1
    print(console, "${total}")
```

`packed` is a layout *contract*. Its fields must be fixed-size scalars
(`Int`/`Float`/`Bool`/`Duration`) or other `packed` types, and a packed list must
stay a confined local — read via `list.length` and `list.at(_, i).field`. Using
one where the flat layout cannot apply — passing or returning it whole, storing it
in a field, comparing, rendering, or `for`-iterating it — is a clean **compile
error** that names the position, never a silent fall-back to the boxed layout you
declared away. The flat representation is applied under `WITCHY_OPT=unbox`; the
contract (and identical results) hold regardless.
