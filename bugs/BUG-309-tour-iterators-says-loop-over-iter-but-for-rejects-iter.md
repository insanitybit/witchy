# BUG-309: tour-iterators.md says a lazy Iter runs when you "loop over it", but `for`/comprehensions reject Iter(a) by design and the chapter never names iter.for_each

Severity: LOW
Status: FIXED
Verified: 2026-07-09 fixed on master 040ce13b
Component: book/src/tour-iterators.md, spec/language.md for-loop sections, docs

## Resolution

`book/src/tour-iterators.md` no longer says a lazy `Iter(a)` runs when users
"loop over it". The tour now names actual iterator consumers: `collect`, `fold`,
`count`, `find`, and `iter.for_each`.

The language behavior is unchanged: `for` remains a list/range loop, and
`iter.for_each` is the library consumer for imperative effects over an `Iter`.

## Problem

Historical problem: `book/src/tour-iterators.md:13` said: "nothing runs until you `collect` it into a
list (or fold, count, or loop over it)" — the chapter that introduces `for`-style
imperative style tells the reader an Iter can be looped over. But the language's
`for` statement and comprehensions reject `Iter(a)` by design: `for x in
iter.range(0, 3):` → "type error: `main`, line 4: `for` expects a List to
iterate: expected `List(?)`, found `Iter(Int)`". Same for a generator call and a
comprehension over an Iter. The only "loop" that works over an Iter is the
library call `iter.for_each` (`spec/stdlib.md:898-900`), which the chapter never
names, while it uses `for x in xs:` three times in its own examples.

The language restriction was deliberate (`spec/language.md` defines `for` over
lists/ranges only; `rfcs/0028-ergonomic-mutable-value-semantics.md:120` explicitly
excludes lazy Iter/generators), so the runtime behavior is not the bug — the
chapter's prose still promises the punted behavior and leads readers straight to
the rejected form. LOW: loud check-time error, no silent divergence.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-gen/t_for_range.witchy
type error: `main`, line 4: `for` expects a List to iterate: expected `List(?)`, found `Iter(Int)`
$ $W check scratch/ultra-gen/t_for_iter.witchy     # for x in nums(): (gen fn)  — same error
# comprehension over an Iter also fails: t_comp_iter.witchy

# controls (parity agree): t_for_dotdot.witchy (for x in 1..4:), t_for_each_ctl2.witchy (iter.for_each)
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-gen/t_for_range.witchy`,
`t_for_iter.witchy`, `t_comp_iter.witchy`; controls `t_for_dotdot.witchy`,
`t_for_each_ctl2.witchy`.

## Code evidence

- `book/src/tour-iterators.md:13` — the "loop over it" claim; the chapter never
  mentions `iter.for_each` yet uses `for x in xs:` in its own examples.
- `spec/language.md:249, 263-270` — `for` defined only over lists/ranges.
- `rfcs/0028-ergonomic-mutable-value-semantics.md:120` — lazy Iter/generators
  explicitly excluded from `for`.
- `spec/stdlib.md:898-900` — `iter.for_each` is the library escape hatch.

## Fix direction

Edit `book/src/tour-iterators.md:13` to name the actual consumers: instead of
"loop over it", say "run it with `iter.for_each`, or `collect`/`fold`/`count` it"
— and add a short `iter.for_each` example so the chapter's imperative-loop
readers have a working path. Optionally improve the `for`-over-Iter diagnostic to
suggest `iter.for_each`/`iter.collect`. No code change required for the doc fix.
