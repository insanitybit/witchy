# Appendix: Performance overview

Witchy performance is primarily expressed in the program's syntax, not in an
environment variable. The syntax tells the compiler what the program promises:
whether a call only reads, consumes, writes back, owns unique storage, chooses a
flat layout, or enters the explicit reference model. The compiler turns those
facts into reuse, fewer copies, smaller representations, and shorter-lived
allocations while preserving value semantics.

This appendix is organized around that source-level question:

| Write this... | To unlock this... | Read... |
|---|---|---|
| `let`, `var`, `own`, `move` | Clear call ownership and write-back behavior | [Syntax and ownership contracts](appendix-performance-ownership.md) |
| `unique`, `local unique`, `frozen` | In-place updates, extraction, and safe sharing | [Syntax and ownership contracts](appendix-performance-ownership.md) |
| `region:` (unstable) | Bulk reclamation for temporary work | [Layouts and allocation lifetime](appendix-performance-layout.md) |
| `packed` | Inline, cache-dense data layout | [Layouts and allocation lifetime](appendix-performance-layout.md) |
| `mode opt`, `&'a T`, `&'a mut T` | Explicit lifetimes, loans, and reference carriers | [Opt-mode References and Lifetimes](opt-mode-references.md) |
| A measured source shape | Evidence for a real workload improvement | [Measuring and diagnosing](appendix-performance-measurement.md) |

The [`WITCHY_OPT` switches](appendix-performance-knobs.md) are still useful,
but mainly for compiler investigation: they isolate a backend pass after a
source-level shape has been chosen. They are not the language's performance
model and normally do not belong in application source or deployment config.

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

The source convention is the important control. `let`, `var`, `own`, and `move`
describe value ownership and call behavior. `unique`, `frozen`, `region:`, and
`packed` add stronger storage facts. `mode opt` permits explicit references and
requires the associated access proofs. A normal file can receive the compiled
tier's ordinary optimizations without adopting explicit reference syntax or
setting a performance environment variable.

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
and generated code, not in language behavior. [Syntax and ownership contracts](appendix-performance-ownership.md)
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
| Backend pass | Remove a proven allocation, check, dispatch, or loop step | The source-level proof pattern does not match |

The compiler should fail closed. If a proof is unavailable, it keeps the
ordinary representation or emits a source-located opt-mode diagnostic. It does
not change value semantics to obtain a speedup.

## Choosing a chapter

Start with [Syntax and ownership contracts](appendix-performance-ownership.md) when you
are choosing function or binding syntax. Read [Layouts and allocation lifetime](appendix-performance-layout.md)
when you are choosing `region:` or `packed`. Read [Opt-mode References and
Lifetimes](opt-mode-references.md) when a reference itself must cross a
boundary. Finish with [Measuring and diagnosing](appendix-performance-measurement.md)
before turning a source change into a performance claim. Use [Compiler switches](appendix-performance-knobs.md)
only when you need to prove which backend pass responded to that source shape.
