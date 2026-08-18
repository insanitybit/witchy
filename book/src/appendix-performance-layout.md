# Performance: layouts and allocation lifetime

The ownership analysis decides when storage may be reused. Layout and lifetime
features decide what that storage looks like and when temporary bytes can be
reclaimed. These features are opt-in contracts or fail-closed compiler proofs;
they never weaken value semantics.

## Regions: reclaim a burst of temporaries

`region:` groups allocations under one lifetime. At the end of the block,
region-born temporaries are reclaimed together. If the block returns a value
that was created inside it, the necessary bytes are copied out. Values owned
before the region may pass through without that copy.

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

The compiler also inserts equivalent loop-watermark resets when it proves that
one iteration's allocations cannot escape. Use an explicit region around
parse-then-summarize work, per-item formatting, or a temporary graph that has a
clear summary boundary. Do not use a region to extend a lifetime: a generator,
task, or closure that outlives the region must not retain its frame.

Useful evidence is `region_rewind_calls` for reclaimed scopes and
`region_copy_bytes` for values copied across the boundary. A high copy count
means the result is large or the region is placed too close to the result; a
zero rewind count means the proof did not match or the block did not allocate
region-owned data.

## Packed values: choose a physical layout

By default, a `List(Point)` stores references to separately represented records.
For a closed fixed-layout record, `packed` makes the record's fields inline and
lets a packed list store one contiguous element buffer:

```witchy
type Point packed:
    x: Int
    y: Int

fn main(console: Console):
    let points = [Point(1, 2), Point(3, 4), Point(5, 6)]
    var total = 0
    var i = 0
    while i < points.length():
        total = total + points.at(i).x + points.at(i).y
        i = i + 1
    console.print("${total}")
```

```text
30
```

The fixed fields may be scalars, nested packed records or tuples, or fixed
layout packed tags. A generic parameter, closure, trait or existential call,
host call, worker/channel transport, rendering boundary, or region result does
not receive a guessed ABI. The compiler retains ordinary boxing or rejects the
boundary according to the source contract.

`unbox` selects canonical descriptors for packed records, packed-containing
tuples, packed lists, and fixed-layout sums. Direct named calls and linked user
modules preserve the descriptor, while unsupported boundaries remain boxed.
Use `packed_alloc_calls` and `packed_alloc_bytes` to confirm that the intended
layout was selected. Do not infer the layout from a faster run alone.

## Destination reuse and reclamation

Two narrower mechanisms build on the same representation proof:

```text
var row = [0, 0, 0, 0]
for i in 0..n:
    row = [i, i + 1, i + 2, i + 3]    // same-shape destination reuse

var state = advance(state, input)     // free dead old state, then reuse it
```

`rc-elide` reuses a compatible same-shape destination. `rc-floor` frees a
confined, never-aliased old allocation at overwrite and makes it available to a
size-class free list. Both retain ordinary ownership behavior when an alias,
escape, or self-reference could observe the old value.

Compare:

| Counter | Meaning |
|---|---|
| `rc_free_calls` | dead allocations returned to the free list |
| `rc_reuse_calls` | allocations satisfied from reusable storage |
| `rc_reused_bytes` | bytes obtained from that storage |
| `live_cells` | live ownership cells at the observed point |
| `heap_bytes` | total tracked heap bytes for the workload |

## Collection updates and extraction

Collection operations preserve the same value and ownership rules:

```text
var queue = [1, 2, 3]
let first = queue.pop()
queue.push(4)

var counts = {"ok": 1}
counts.insert("ok", 2)
```

On a unique root, `pop`, `push`, insert, remove, and field updates can carry
the spine token and avoid copying unrelated elements. On an aliased root,
normal mode uses copy-on-write. In opt mode, an active loan or lost uniqueness
is a source-level error where the contract requires exclusivity. Extraction
counters distinguish the leaf returned from the collection spine:

```text
extract_copied_bytes
extract_searches
indirect_ownership_calls
```

Dictionary indexing remains O(1) through its hidden open-addressing index while
iteration order stays insertion order. A benchmark should measure lookup and
iteration separately; a large deterministic iteration can hide an improved
lookup path.

## Layout decision checklist

Before choosing a layout or lifetime feature, answer:

1. Is the value fixed-layout and closed, or can a generic/dynamic boundary see
   it?
2. Can another binding, reference, task, closure, or host observe the old
   storage?
3. Does the result outlive the region or owner that created it?
4. Which counter or generated-code artifact proves the intended path fired?
5. Does the interpreter, ordinary Wasm, and forced-copy configuration agree?

If any answer is uncertain, keep the ordinary representation and measure the
next concrete bottleneck. The safe fallback is part of the design.
