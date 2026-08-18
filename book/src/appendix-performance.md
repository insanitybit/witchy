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

## The compiler lever: `WITCHY_OPT`

The source-level contracts above and the compiler's optimization switches are
different controls. `mode opt` changes what a source file must prove; it makes
ownership conventions explicit and turns a missed in-place proof into an error.
`WITCHY_OPT` selects which semantics-preserving backend passes are enabled for
one compilation. A normal file may be compiled with every pass enabled, and an
`opt` file may be compiled with a pass disabled for differential debugging. No
setting changes the language result or trap behavior.

For compiler and implementation details, see the [performance
spec](https://github.com/insanitybit/witchy/blob/master/spec/performance.md) and
[performance modes RFC](https://github.com/insanitybit/witchy/blob/master/rfcs/performance-modes.md);
this appendix is the user-facing knob guide.

There are two named shipping modes:

```sh
# release is also the default when WITCHY_OPT is unset
WITCHY_OPT=release witchy run app.witchy

# debug is the no-optimization reference configuration
WITCHY_OPT=debug witchy run app.witchy
```

`release` and `all` enable the complete hardened registry. `debug` and `none`
disable it. For a focused comparison, start from release and remove one pass:

```sh
WITCHY_OPT=release witchy stats app.witchy > stats.release
WITCHY_OPT=release,-inplace witchy stats app.witchy > stats.copy
WITCHY_OPT=release,-unbox witchy stats app.witchy > stats.boxed
```

The grammar is comma-separated. `-name` removes a pass, `+name` adds one, and a
leading `none,name` creates an allowlist. `release`, `debug`, `all`, and `none`
are only valid as the first token. The setting is process-wide and is read
before compilation; use a separate command invocation for each comparison.

### The 14 registered passes

Every switch below is a real consumer in the compiler, not a promised future
optimization. The default is **on** for every entry. Turning one off is useful
for a controlled comparison or a regression reproducer, not as a way to change
program meaning.

#### `inplace`: uniqueness-driven mutation

With `inplace`, an unaliased `var` accumulation can reuse its buffer: list push,
string concatenation, dictionary update, record update, and related write-back
operations avoid rebuilding the whole value. `own`/`move` calls can carry the
same ownership token across a boundary. With `-inplace`, the compiler takes the
copy-correct path and re-owns at accumulation sites. Compare `reowns`,
`heap_bytes`, and `extract_copied_bytes`; a clean accumulation should have
substantially fewer re-owns with the pass enabled.

This is proof-driven, not a promise that every `var` is in place. An alias,
active loan, unknown escaping call, or unsupported projection correctly forces a
copy in the enabled build. In `mode opt`, the same missing proof is a diagnostic
instead of a silent cliff.

#### `views`: confined borrowed slices

`views` lets the compiler represent a confined read-only slice as a view of its
owner instead of copying the selected elements. The view must be consumed within
the owner's valid lifetime; it cannot escape through a return, closure, task, or
channel. `-views` materializes the slice, which is the useful differential
oracle for copy cost. Compare `heap_bytes` on a large `list.slice` workload
(and `packed_alloc_*` when the slice element type is packed) while checking
identical output.

This switch does not make an arbitrary slice safe to return. Lifetimes and
borrow checking remain source/type-checker rules in `mode opt`; the pass only
chooses the representation after those rules succeed.

#### `sroa`: scalar replacement of aggregates

`sroa` replaces a non-escaping record or tuple with scalar locals when all uses
are analyzable field/index operations. A per-iteration temporary can therefore
stay out of the heap, including a record whose scalar fields are updated. With
`-sroa`, the same value uses its ordinary heap representation. Compare
`heap_bytes` on a loop that constructs and consumes a confined aggregate.

SROA is deliberately escape-driven. Returning the aggregate, storing it in an
owned container, passing it through an unsupported boundary, or taking an
unknown alias keeps the heap representation even when `sroa` is enabled.

#### `region`: arena and loop-watermark reclamation

`region` enables bulk reclamation at explicit `region:` exits and at compiler-
proven escape-free loop iterations. Temporaries allocated inside the scope are
rewound together when no value from that scope escapes. `-region` leaves those
allocations live until a later structural reclamation point. Compare
`heap_bytes`, `region_rewind_calls`, and `region_copy_bytes` on a scratch-heavy
loop.

The value leaving a region remains correct: region-born data is copied out when
needed, while values already owned outside the region can be passed through.
`region` is not a general lifetime extension and generators cannot yield from a
region whose frame would outlive it.

#### `rc-elide`: confined same-shape reuse

`rc-elide` reuses the buffer of a confined heap local reassigned from a compatible
same-shape list literal or record constructor. A list grows its retained
capacity when necessary and can reuse it on later assignments; a record
overwrites its fixed slots. `-rc-elide` allocates a fresh value for those
reassignments. The relevant evidence is bounded `heap_bytes` and the allocation
counters in `witchy stats`.

The escape/uniqueness analysis must prove that the old buffer is not observable.
A self-referential assignment or an alias falls back to a fresh allocation.

#### `fold`: constant folding and propagation

`fold` evaluates compile-time arithmetic, comparisons, boolean/bitwise
expressions, unary literals, and literal string concatenation. It also
propagates immutable literal `let` bindings into later expressions, which can
remove allocations the Wasm mid-end cannot see. `-fold` evaluates the same
expression at runtime. Output must be identical; `heap_bytes` can expose a
removed constant-concatenation allocation.

Folding preserves Witchy's defined wrapping integer and IEEE floating-point
behavior. It never treats a runtime value as constant merely because a current
run happened to produce one value.

#### `unbox`: packed and unboxed layouts

`unbox` selects canonical flat layouts for closed `packed` records, packed
tuples, `List(Packed)`, and fixed-layout packed sums. A packed list stores inline
elements in one `[length][capacity][elements...]` buffer instead of an array of
boxed record pointers. Direct calls and supported linked-module boundaries keep
the same layout descriptor. `-unbox` uses the uniform boxed representation.

Use `packed_alloc_calls` and `packed_alloc_bytes` to measure descriptor-owned
storage. The layout is only selected when the type and boundary have an exact
physical ABI; generic, closure, trait, worker/channel, rendering, and other
unsupported crossings fail closed or use the ordinary representation.

#### `rc-floor`: free-at-overwrite reclamation

`rc-floor` gives a confined, never-aliased heap object an allocation header and
returns its old buffer to a size-classed free list when an overwrite makes it
dead. The next compatible allocation can reuse that storage. This is broader
than `rc-elide`: it covers a newly produced replacement such as `x = f(x)`, not
only a same-shape literal. `-rc-floor` leaves that overwrite garbage for the
other reclamation mechanisms. Compare `rc_free_calls`, `rc_reuse_calls`,
`rc_reused_bytes`, and `live_cells`.

The floor is an optimization over proven confinement, not tracing collection.
Values that escape or may be aliased retain their ordinary ownership behavior;
the pass must not free storage that remains observable.

#### `wasm-opt`: Binaryen post-processing

`wasm-opt` runs the available Binaryen post-pass over emitted Wasm during a cold
compile. Its validated result is cached, so warm runs do not repeatedly pay the
Binaryen invocation. If `wasm-opt` is not installed, the pass is a graceful
no-op. `-wasm-opt` is useful when isolating cold-start cost or diagnosing a
post-pass issue; it is not a semantic or ownership switch. Measure cold and
warm compile/run paths separately because the post-pass cost is paid only on a
cache miss.

#### `direct-call`: known closure devirtualization

`direct-call` changes a closure call from `call_indirect` to a direct Wasm call
when local analysis proves that one lambda is bound and never reassigned. This
removes the code-index dispatch and can expose the body to further Wasm
optimization. `-direct-call` retains indirect dispatch. The proof is a codegen
shape change, so ordinary allocation counters may remain unchanged; use
generated Wasm/codegen diagnostics and output parity as the evidence.

#### `bounds-elide`: proven counted-loop checks

`bounds-elide` removes the list index bounds check inside the exact proven shape
`for i in 0..list.length(xs)` when `xs` is not reassigned. The loop's counter
invariant supplies the proof. `-bounds-elide` keeps the check on every access.
The optimization is conservative: a shape miss retains the check, and a
dynamic index is never guessed safe. Compare emitted code and retain the
interpreter/Wasm output and trap parity tests.

#### `closure-elide`: non-escaping closure environments

`closure-elide` avoids allocating a heap environment for a closure that is used
only as a direct callee in its creating scope and whose captures are not
reassigned. Captures become extra lambda arguments. `-closure-elide` keeps the
boxed environment path. Any uncertain escape, reassignment, indirect storage,
trait use, or host crossing remains boxed even when the pass is enabled. The
proof is a code shape plus heap check, not merely a faster-looking benchmark.

#### `loop-unroll`: four-lane counted-loop unrolling

`loop-unroll` emits a four-lane body for proven-safe counted ranges and packed
cursor walks, leaving a guarded scalar cleanup for the tail. `-loop-unroll`
emits one scalar iteration. The pass trades code size and compile time for
fewer loop-control operations; it is most useful for long, hot, predictable
loops and may be neutral or worse for tiny loops. Verify output, bounds/trap
parity, and matched workload timings rather than assuming a speedup.

#### `direct-storage-var`: direct `var` write-back

`direct-storage-var` commits a `var` call result directly into a proven-disjoint
whole local instead of reconstructing it through a scratch destination. The
callee already returns the exact owner the local must hold. `-direct-storage-var`
keeps the staged reconstruction oracle. The pass requires evaluate-once,
non-overlap, no live alias/view, no callee escape, identical representation, and
valid whole-local write-back proofs. `direct_storage_var_accesses` counts direct
commits; a projection, live loan, or uncertain alias correctly falls back.

### Measuring knobs without fooling yourself

`witchy stats` reports deterministic operation counts, not wall-clock timings.
It compiles and runs the same Wasm program under the active `WITCHY_OPT` setting
and prints the program output plus heap, ownership, region, packed-layout, RC,
and extraction counters. A useful comparison changes one lever and keeps the
source, input, backend, memory budget, and command environment fixed:

```sh
WITCHY_OPT=release witchy stats bench.witchy > /tmp/release.stats
WITCHY_OPT=release,-inplace witchy stats bench.witchy > /tmp/no-inplace.stats
diff -u /tmp/release.stats /tmp/no-inplace.stats
```

For a shape-only pass such as `direct-call` or `bounds-elide`, counters may not
move. Inspect emitted Wasm or the focused codegen test and still compare output
and traps. For throughput or latency, use a paired timing tool around the same
two commands, report cold and warm runs separately, and repeat enough to show
variance. Never infer that a pass fired merely because a run got faster, and
never infer a semantic difference from a counter difference without checking
the output and parity corpus.

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

## Borrowed views and explicit references (`mode opt`)

A `mode opt` module may return a read-only **view** of a value it was given,
instead of copying it. The reference type names the lifetime directly as
`&'a T`; the value is the same one at runtime (a reference has no independent
logical value), so this changes only *when* a copy is made, never the observable
result:

```witchy
mode opt

fn first(let text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var s = "borrowed, not copied"
    let view = first(&s)
    console.print(*view)
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

fn first(let text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var s = "borrowed"
    let kept = first(&s).owned()
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
element owner, which is conservative and correct. Overwrite, drop, and nested list
containers are now supported; relation-erasing boundaries remain rejected until an
owned-companion conversion explicitly materializes the value.

The borrowed-shell/iterator workload is compiled and interpreted against the same
checked-heap root facts, including `return`, branch/loop exits, `?`, and
checked-heap/UAF stress (`WITCHY_HEAP_CHECK=1`, `WITCHY_UAF_CHECK=1`).
Parser and iterator shells must keep ownership rooted only when needed and close
those roots before host release; they are explicitly validated as zero
materialization in shipped tests via `__witchy_packed_alloc_calls == 0` and
`__witchy_packed_alloc_bytes == 0`.
