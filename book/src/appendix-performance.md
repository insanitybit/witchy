# Appendix: Performance overview

Witchy has value semantics: a callee cannot mutate storage that the caller can
still observe. That promise is the foundation for the compiled tier's
optimizations. When the compiler proves that a value has one effective owner,
it can update that value in place without changing the program's result.

This appendix is a guide to the contracts, passes, layouts, and measurements
behind that behavior. It is deliberately split by question:

| If you want to know... | Read... |
|---|---|
| What `let`, `var`, `own`, `unique`, `frozen`, and `mode opt` promise | [Ownership contracts](appendix-performance-ownership.md) |
| What each `WITCHY_OPT` switch does and what makes it fire | [Optimization knobs](appendix-performance-knobs.md) |
| How regions, packed values, lists, and reclamation affect memory | [Layouts and allocation lifetime](appendix-performance-layout.md) |
| How to compare builds and interpret counters | [Measuring and diagnosing](appendix-performance-measurement.md) |
| How explicit references fit into opt mode | [Opt-mode References and Lifetimes](opt-mode-references.md) |

## Start with the default

Write ordinary value-oriented Witchy first. The compiler already analyzes
unannotated functions and applies any proof that does not require a source
contract. Add an ownership convention when it states something true about the
boundary, not merely because a benchmark is slow.

```text
default owned value       fn render(xs: List(Int)) -> String
read-only call borrow     fn inspect(let xs: List(Int)) -> Int
ownership transfer        fn consume(own xs: List(Int)) -> Int
caller write-back         fn normalize(var text: String) -> Nil
```

The source convention and the backend switch are separate controls. `let`,
`var`, `own`, and `move` describe value ownership and call behavior. `mode opt`
requires stronger access proofs and permits explicit references. `WITCHY_OPT`
selects semantics-preserving backend passes for one compilation. A normal file
can use release optimizations without adopting explicit reference syntax.

## A small ownership pipeline

An `own` parameter can carry one collection buffer through a call boundary. The
caller consumes its old binding with `move`, and the callee returns the updated
owner:

```witchy
fn grow(own xs: List(Int), n: Int) -> List(Int):
    var out = move xs
    out.push(n)
    out

fn main(console: Console):
    var xs = [0]
    xs = grow(move xs, 1)
    xs = grow(move xs, 2)
    console.print("${xs.length()}")
```

```text
3
```

The optimized path may retain the collection's capacity across both calls. The
copy-correct path produces the same `3`; the difference is visible in counters
and generated code, not in language behavior. [Ownership contracts](appendix-performance-ownership.md)
explains why this is sound and when an alias or escape forces a copy.

## The performance model in one table

| Mechanism | Typical benefit | What can defeat it |
|---|---|---|
| Unique `var` update | Reuse a list, string, dictionary, or record buffer | An alias, live loan, or unknown escape |
| `own` plus `move` | Carry an owner through calls without re-owning | Keeping another usable binding or passing through an unsupported boundary |
| `let` input | Prove that a helper reads without taking ownership | Returning, storing, or otherwise letting the borrow escape |
| `region:` | Reclaim many temporaries at one scope exit | Returning region-born data without copying it out |
| `packed` | Store fixed-layout records inline | Generic, dynamic, trait, host, or worker boundaries without a fixed ABI |
| Explicit opt-mode references | Avoid copies across a proven lifetime | A conflicting loan, escape, suspension, or relation-erasing boundary |
| `WITCHY_OPT` pass | Remove a proven allocation, check, dispatch, or loop step | The pass's proof pattern does not match |

The compiler should fail closed. If a proof is unavailable, it keeps the
ordinary representation or emits a source-located opt-mode diagnostic. It does
not change value semantics to obtain a speedup.

## Choosing a chapter

Start with [Ownership contracts](appendix-performance-ownership.md) if a
`witchy check` message mentions copies, uniqueness, `var`, or a live loan. Read
[Optimization knobs](appendix-performance-knobs.md) when you need to isolate a
compiler pass. Read [Layouts and allocation lifetime](appendix-performance-layout.md)
when the counters point to heap, packed, region, or reclamation work. Finish
with [Measuring and diagnosing](appendix-performance-measurement.md) before
turning a counter change into a performance claim.
