---
rfc: 0181
title: "Inline aggregate layout, mutable places, and write summaries"
status: proposed
created: 2026-10-09
tracking: >
  Fixed-size records stored inline in lists and records; `var` on a
  projection as the second-class mutable place; per-function field and index
  write summaries with disjointness facts. Companion to RFC-0178. Amends the
  frame condition of RFC-0177.
---

# RFC-0181: Inline aggregate layout

## Summary

A `List(Point)` today is a list of pointers to heap records. This RFC stores
fixed-size records inline, so the list is `[x, y, x, y, ...]`, and records
inside records likewise. Reading `xs[i].x` becomes one lane load. Writing
`xs[i].x = 3` on a unique list becomes one store, where today it allocates a
copy of the record and swaps the pointer unless the record's uniqueness is
proven.

Two further pieces make in-place writes reach through calls:

- **`var` on a projection**: `bump(var xs[i].pos)` passes the place inside
  the list, not a copy of the element. Parameter-only, so it cannot escape.
- **Write summaries**: the compiler records, per function, which fields and
  indices of each `var` parameter it writes. A caller keeps a live view of
  the untouched part across the call, and two `var` places into one list
  coexist when their indices are proven distinct.

Nothing here is a new ownership mechanism. Storage is a static layout
decision per type; in-place writes use the uniqueness the ownership analysis
already computes; summaries are the existing escape analysis at field and
index granularity.

## Motivation

The remaining gap against Go on `word_count`, `fannkuch` and `nsieve`, and
the goose results that beat Rust, are explained by representation. goose
keeps nested data inline and never chases a pointer inside a value. witchy
pays, per hot-loop write to an aggregate element, either a uniqueness proof
or a copy-and-swap allocation, plus a retain and release per reference.
milo's benchmarks put the inline-versus-arena gap at 20-30% on pointer
chasing and the allocation-per-write cost above that; regions (RFC-0178)
remove the release, and only inline layout removes the allocation.

Value semantics make this cheap to adopt: a record stored in a list is
already semantically a copy, and identity is unobservable, so the layout can
change with no change to any program's meaning.

## 1. Inline storage

A type is **inline-eligible** when it is a record, tuple or sum whose fields
are scalars, pointers (String, List, Dict, closures, capability tokens) or
inline-eligible types, with no `must` obligation and no view inside, and
whose size is under a threshold (initially 64 bytes; measured). A list or
record whose element or field type is inline-eligible stores it inline.

Rules:

- The decision is **static and per type**. No runtime fact about a list is
  consulted. Identity is unobservable under value semantics, so no program
  can tell.
- Recursive types, sums whose variants differ in size beyond the threshold,
  and anything carrying a view or `must` stay boxed. Recursive shapes are
  RFC-0178's job.
- Strings are not stored inline inside records (goose's variable-size
  layout). That would make `String` layout-dependent everywhere.
- Sums store tag plus the largest variant inline (fixed mode). Variable-size
  storage for append-only lists of sums (goose's variable mode) is a
  follow-up with its own measurement.
- Packed scalar lists are unchanged; this extends the same packed path to
  fixed records.

Reads: `xs[i]` materializes the record only if the whole value is used;
`xs[i].x` is a lane load through the existing SROA. `&xs[i]` is an element
view over the inline lanes and copies nothing. `for p in xs` iterates lanes.

Writes: `xs[i] = p` stores the lanes. `xs[i].x = 3` stores one lane. Both
require the list to be unique at the write, which is the rule today; a
shared list is copied once (as today) and then written.

## 2. `var` on a projection

```sh
fn bump(p: var Point):
    p.x += 1

bump(var pts[i])            // the lane set of element i, in place
bump(var pts[i].pos)        // a nested record's lanes, in place
```

`var` already is a second-class mutable reference restricted to one call:
move-in, move-out, write-back. This RFC lets the argument be a **place**
(a local, a field chain, an index, or a mix) instead of a whole local. The
callee's signature is unchanged. The caller's obligations:

- the root of the place must be a unique `var` binding at the call (as for
  any in-place write);
- no other live view or `var` place of an overlapping part of the same root
  may exist across the call (§3 decides overlap);
- the place must be inline or a pointer field; a boxed element is passed as
  today (copy, call, write back) and the compiler says so on request.

It is parameter-only: a `var` place cannot be returned, stored, captured or
sent, which is exactly the restriction that makes milo's `&mut` need no
lifetimes. First-class mutable references are not proposed.

## 3. Write summaries and disjointness

For every function, the compiler records a **write summary**: for each
`var` parameter, the set of field paths and index expressions the body may
write, or "whole" when it cannot narrow. Summaries are:

- **inferred inside a module** from the body, bottom-up over the call graph,
  computed once and keyed by the body digest like SafeWIR verdicts, so a
  caller reads its callee's summary and never re-analyzes the body.
  Recursive groups get "whole". This is linear in code size and needs no
  global re-annotation.
- **written at boundaries** where the body is not visible: trait methods,
  function-value types, and `pub` functions of a package. The spelling is
  `var Point.{x}` for "writes `x` only". std, examples and compiler-owned
  code write them explicitly wherever a `var` parameter is narrower than
  whole, in line with the rule that std stays explicit about conventions.
- **checked, not trusted**: the SafeWIR checker verifies a body against its
  declared or inferred summary. A wrong summary is a refusal.

**Disjointness.** Two places into the same root are disjoint when their
field paths diverge, or when their index expressions are proven distinct by
the implied-guard lattice (constants, a loop index against a shifted copy,
an `i != j` guard, `i + n` with `n` known nonzero). This adds the relation
"distinct" beside "in range" in the existing facts; nothing new in shape.

With both, the caller may:

- hold `&p.y` live across `bump(var p)` when `bump` writes `p.x` only;
- hold `var pts[i].x` and `var pts[j].x` together when `i != j` is known;
- mutate a `var` list while an element view of a disjoint index is live,
  which is the refusal in BUG-647 today.

## 4. Amendment to RFC-0177

RFC-0177 §3 says witchy needs no `modifies` clause because a call writes
only its `var` parameters. That stays true, and §2 of this RFC makes the
written place finer than a parameter, so the frame of a call is now: the
`var` places it receives, narrowed by the callee's write summary. The
verifier treats a summary as an invariant to prove from the body (like
`unique`), and disjointness as an ordinary proof goal. Per RFC-0177 §4, the
optimizer uses a disjointness fact only when established for all inputs;
a bounded result compiles the overlap check at runtime.

The summaries must live in the SafeWIR semantics the verifier evaluates,
not only in the frontend analysis, or the two engines diverge. They land as
checked facts.

## Acceptance

Measured with the kernel A/B harness (allocations, retains, releases per
iteration).

| # | Criterion | Evidence |
|---|---|---|
| 1 | A `List(Point)` read loop allocates nothing per iteration and matches its boxed result | Harness counters; expected output on admitted unoptimized Wasm |
| 2 | `xs[i].x = 3` on a unique `var List(Point)` is one store: 0 allocations/iter | Harness counter; a structural check that the lowered body has no `Alloc` in the loop |
| 3 | The same write on a shared list copies the list once, then stores | Counter shows exactly one allocation per shared write sequence |
| 4 | `bump(var pts[i])` writes in place with no element copy; the result matches the copy-call-write-back form | Counters and expected output |
| 5 | A boxed element passed as a `var` place compiles with the copy form and reports it on request | Diagnostic test |
| 6 | A declared summary narrower than the body is refused by the checker | Forged-module and source tests |
| 7 | `&p.y` live across `bump(var p)` with summary `{x}` is accepted; with summary `whole` it is refused | Typeck tests |
| 8 | `var pts[i].x` and `var pts[j].x` coexist under an `i != j` guard and are refused without it | Typeck tests |
| 9 | BUG-647's program (mutating a `var` list while a disjoint element view is live) compiles and matches its expected result | Expected output on admitted unoptimized Wasm |
| 10 | Existing packed scalar lists and every example test are unchanged | Full example matrix |
| 11 | Compile time of the kernel and example corpus does not regress beyond noise | `compile-bench.sh` paired against master |

## Non-goals

- First-class mutable references, `&alias`, `Pin`, `Move`/`!Move`.
- Strings inline inside records.
- Variable-size sum storage (follow-up).
- Changing which types are `unique` or `frozen`; this RFC consumes those
  facts and adds none.

## Prior art

goose (inline nested data, variable-mode enums, references into array
elements); milo (second-class `&mut`, arena handles); Zeta's borrow-checker
note (multi-place references `Vec3f.{&mut x}` as write summaries, index
disjointness from constants, loop structure, guards and arithmetic). witchy
takes the write-summary and disjointness ideas without the first-class
references they were designed to tame, because value semantics already
removed the aliasing problem they exist to solve.
