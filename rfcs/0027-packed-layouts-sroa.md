---
rfc: 0027
title: packed layouts and escape-driven SROA
status: proposed
created: 2026-06-28
tracking:
---

# RFC-0027: Packed layouts and escape-driven SROA

## Summary

Add the one optimization axis witchy has **no** knobs for today: data
*representation*. Two changes, both opt-in and both pure consumers of the escape
oracle ([0024](0024-unified-facts-lattice.md)): (1) a `packed` qualifier on a
type that makes `List(Point)` a flat `[len][cap][x0,y0,x1,y1,…]` buffer instead
of an array of pointers to boxed records — the only lever that changes cache
*asymptotics*; and (2) escape-driven **SROA** (scalar replacement of
aggregates), which lowers a non-escaping record/tuple into individual WASM locals
so it never touches the heap at all. Both are layout choices, invisible to
program semantics, gated so normal-mode codegen stays simple.

## Motivation

[RFC-0017](0017-codegen-performance.md) traces witchy's remaining gap to Go/Rust
to one structural choice: the **uniform 8-byte slot with boxed aggregates**. A
record, tuple, or `List(Point)` element is an i32 pointer to a separate heap
object. That uniformity buys simple codegen and one equality/format/copy story —
but it is the dominant cost on cache-bound workloads (nsieve, fannkuch,
knucleotide in the bench suite), costing up to 8× the memory traffic and
defeating SIMD. Every other knob in this cluster
([0025](0025-frozen-deep-immutability.md), [0026](0026-unique-qualifier.md))
reduces *copying* and *allocation*; none changes *layout*, which is where the
order-of-magnitude on numeric/struct-heavy code actually lives.

witchy has the facts to do this safely now: monomorphization already specializes
generic code per concrete type, and the escape oracle already knows which
aggregates never leave their frame. What is missing is the representation choice
itself, and a knob to opt into it where its cost (touching every host function
that reads guest memory) is worth paying.

## Design

### Part 1 — `packed` layouts (unboxing)

A type declared `packed` is stored inline wherever it appears in a container,
rather than behind a pointer:

```witchy
type Point packed:
    x: Int
    y: Int

// List(Point) is now [len][cap][x0,y0,x1,y1,…], not [len][cap][p0,p1,…]
// access is base + i*stride + field_offset — cache-dense, SIMD-eligible
```

Rules:

- `packed` requires a **fixed, statically-known size**: all fields are scalars
  (`Int`/`Float`/`Bool`/`Duration`) or other `packed` types. A field of variable
  size (`String`, `List`, a non-`packed` record, a sum type with payloads of
  differing size) makes the type unpackable — a check-time error naming the
  offending field.
- Layout becomes part of monomorphization: `List(Point)` and `List(BoxedThing)`
  are different *representations*, not just different specializations. This is the
  invasive part — it touches every host function and helper that reads guest
  memory (`at`, iterate, equality, `show`/format, message marshaling,
  `$rcopy`), each of which must consult the element's layout.
- Semantics are **unchanged**: `==`, `${...}`, copy-out, and pattern matching all
  produce identical results on a packed and an unpacked value. This is the parity
  contract and is enforced by running the suite with packing forced off (a
  representation analog of the forced-copy differential mode).

Because of the breadth, `packed` is **gated**: available in `mode opt` files (and
for `unique`/`frozen` values whose layout is statically pinned), not in
normal-mode codegen, so the general path keeps its single uniform
representation and simple host ABI.

### Part 2 — escape-driven SROA

Independently of `packed`, a record or tuple that the escape oracle proves
`confined_to ⊑ Frame` ([0024](0024-unified-facts-lattice.md)) never needs a heap
object at all: its fields become individual WASM locals.

```witchy
fn dist(a: Point, b: Point) -> Int:
    let d = Point(b.x - a.x, b.y - a.y)   // never escapes → d.x, d.y are just locals
    d.x * d.x + d.y * d.y
```

- The trigger is purely a fact query (`confined_to ⊑ Frame` and a fixed shape),
  so SROA is automatic, not a knob — no annotation needed. It is the direct
  analog of Rust stack allocation and the natural payoff of the unified oracle.
- A value that conditionally escapes (returned on one branch) is materialized to
  the heap at the escape point; SROA applies to the non-escaping portion of its
  life. (Sharpening this past whole-value granularity is the field-sensitivity
  follow-on noted in [0024](0024-unified-facts-lattice.md).)
- Risk is low: the lambda-capture escape scan and uniqueness pass already supply
  the needed facts; this is medium effort, low risk per
  [performance-modes.md](performance-modes.md) tier 2.

### Interaction with the rest of the cluster

- **SROA needs no new analysis** — it is the first real consumer that proves
  [0024](0024-unified-facts-lattice.md) pays for itself, so it should land first
  in this RFC.
- **`packed` composes with [0026](0026-unique-qualifier.md)**: a `unique
  packed List(Point)` is the near-Rust case — flat layout, in-place reuse,
  cache-dense, SIMD-eligible — and is the headline `mode opt` capability.
- **`packed` composes with [0025](0025-frozen-deep-immutability.md)**: a `frozen
  packed` table is a shareable, cache-dense, read-only array — the ideal lookup
  structure.

## Alternatives

- **wasm-gc structs/arrays** (weighed in [performance.md](../spec/performance.md) Phase 4
  and [0016](0016-reference-counted-memory.md)): would give typed layouts via the
  engine, but rewrites the value representation *and* every host function that
  reads guest memory, and surrenders the arena's bulk-free advantage on the
  workloads witchy wins. `packed` gets the layout win within the existing linear-
  memory + arena model, opt-in, without a second collector.
- **Always unbox** (no knob). Rejected: the uniform representation is what keeps
  normal-mode codegen, the host ABI, and the eq/copy/format machinery simple and
  parity-safe. The cost of unboxing everywhere is paid in compiler complexity on
  every type, including the ones where it does not matter.
- **Infer `packed` instead of declaring it.** Layout is observable through
  performance only, so inferring it is sound — but the host-ABI cost is large
  enough that we want it where the programmer asked, in `mode opt`, not silently
  everywhere. SROA *is* the inferred case (it is invisible and frame-local);
  `packed` is the declared case (it changes the heap representation a host
  function sees).

## Drawbacks

- `packed` is the most invasive change in this cluster: every host function and
  stdlib helper that walks guest memory must become layout-aware. This is why it
  is gated and staged last.
- Two representations for one type means two code paths in those helpers (boxed
  and packed), which is real maintenance weight. Monomorphization keeps them out
  of the normal path, but the helpers themselves grow.
- SROA on partially-escaping values needs a materialization point chosen
  correctly or it is unsound; whole-value granularity is safe but leaves
  performance on the table until field-sensitivity lands.
- SIMD eligibility is stated as a benefit but is a separate follow-on (the
  `relaxed-simd` work in [performance.md](../spec/performance.md) Phase 2); `packed` is
  the *precondition*, not the SIMD work itself.

## Prior art

- [RFC-0017](0017-codegen-performance.md) §O1 (unboxed/monomorphized layouts) —
  this RFC is its concrete proposal.
- [performance-modes.md](performance-modes.md) "representation tiers" — tiers 1
  (unboxing) and 2 (SROA); this RFC realizes both.
- Rust `repr(C)`/`repr(packed)` and Swift `@frozen` — declared layout as a
  performance/ABI contract.
- Classic SROA / scalar replacement (LLVM `sroa`); escape analysis →
  stack allocation (Java HotSpot, Go).

> 2026-06-29: **Part 2 (SROA) shipped; Part 1 (packed) — analysis increment 1
> landed, via INFERENCE for the confined case.** SROA (scalar replacement of
> frame-confined records/tuples) is implemented and gated `WITCHY_OPT=sroa`.
>
> For packed layouts, rather than start with the declared `packed` qualifier
> (which needs parser + typeck packability checking + `mode opt` gating infra that
> does not exist yet, and makes every host fn reading `List(<record>)` layout-aware
> — the most invasive path), the first increment delivers the SAME cache-density
> win by INFERENCE for the confined case, mirroring how confined views (RFC-0028)
> deliver zero-copy borrows without a `View` type. `escape::confined_record_list_candidates`
> (committed, additive, no consumer yet) identifies `let xs = [P(..), ..]` read
> ONLY via `list.length(xs)` and `list.at(xs, i).field` (never whole, no element
> taken whole, never reassigned) — a list that can become a flat packed buffer with
> each `at(i).field` lowered to a direct slot read. NEXT (increment 2, the hard +
> risky part, warrants fresh context): the gated codegen consumer — type-level
> packability filter (element record fixed-size all-scalar), flat-buffer
> construction at the `let`, `list.at(xs,i).field` → slot read, `list.length` →
> count, gated by a `packed`/`unbox` `WITCHY_OPT` lever (re-added with this
> consumer + a `witchy stats` heap-drop counter + the differential sweep). The
> declared `packed` qualifier (for non-confined / `pub`-API / host-visible layouts)
> remains future work on top. RFC stays `proposed` until the codegen lands.

> 2026-06-29 (later): **Packed increment 2 (the gated codegen) landed.** A confined
> record-list candidate whose element type is packable (all fixed-scalar fields,
> `is_packable_record`) now compiles, under opt-in `WITCHY_OPT=unbox`, to ONE flat
> inline buffer — `let xs = [P(a,b), ..]` → `mk{N*nfields}(N, a0,b0,a1,b1,…)` (header
> = element count, so `list.length` is unchanged; reuses the checked-heap-correct
> `$mkN` allocator, no new allocation path), and `list.at(xs,i).field` → a direct
> i64-slot load at `xs + 4 + (i*nfields + j)*8` (one load, no pointer deref). The
> per-field slot representation is byte-identical to a boxed record, so it is just
> flattened. Measured: a 10×2-field list goes from 4 allocations (3 records + list,
> the de-opt path) to 1, a ~120-byte heap drop (`stats::packed_record_list_uses_one_flat_buffer`)
> with identical output; the differential sweep gained a packed program so
> `unbox`-on (`all`) == off == interp. The `unbox` lever is re-registered WITH this
> consumer + counter (RFC-0030's no-phantom-levers rule). STILL FUTURE: the declared
> `packed` qualifier (parser + typeck + `mode opt` gating + host-visible layout
> agreement) for non-confined / `pub`-API cases; the inference covers the local
> confined case only. RFC stays `proposed` until the declared qualifier lands.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
