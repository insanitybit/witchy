# BUG-366: Iterator skip adapters recurse per skipped element

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on branch fix/iter-skip-loops
Component: `std/iter`, generated stdlib docs, iterator reliability, backend parity risk

## Current status

Current `std/iter` has moved the stack-depth-sensitive adapter and consumer
paths to the safer loop-driven pattern. `find`, `any`, `all`, `last`,
`position`, `min`, `max`, and `drop` were already iterative; the remaining lazy
skip adapters are now iterative too:

- `filter_step` loops through rejected elements inside one pull.
- `filter_map_step` loops through `None` results inside one pull.
- `drop_while_step` loops through the dropped prefix.
- `flat_map_step` loops through mapped empty inner iterators until it finds the
  first yielded inner value or reaches outer exhaustion.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-iter cargo test iterator_skip_adapters_handle_long_prefixes_on_both_backends -- --nocapture
```

The separate channel join/cancel fan-out recursion is tracked by BUG-396 rather
than this iterator row.

## Historical problem

`std/iter` presents lazy iterators as the public collection story for generators:
adapters compose without intermediate lists, and infinite iterators are described
as fine when bounded by `take`, `take_while`, or a successful `find`. Several of
the core adapter/consumer implementations do not preserve that operational
contract for long ordinary inputs: they recurse once per skipped or consumed
element instead of using the iterative driver style already used by `collect`,
`fold`, and `for_each`.

The most visible cases:

- `filter` and `filter_map` recurse through every rejected element inside one
  `next` pull, so `next(iter.filter(iter.range(0, n), fn(_): false))` or a long
  rejected prefix can build one stack frame per input element before returning.
- `drop_while` recurses through every dropped prefix element inside one pull.
- `find`, `any`, `all`, and `position` recurse through every non-matching
  prefix, despite their docs saying they are safe on an unbounded iterator once
  a match exists. A match at position 100000 is bounded semantically but still
  stack-depth-sensitive operationally.
- `last`, `min`, and `max` recurse to exhaustion, unlike `fold`/`count`/`sum`,
  which already use a `while` loop.
- `drop(it, k)` recurses immediately while constructing the dropped iterator,
  so a large `k` consumes stack before the returned iterator is used.

This is not just a performance nit. The release-facing iterator story should not
make common bounded uses depend on native/wasm stack depth, especially when the
same file demonstrates the safer implementation pattern with explicit `while`
loops. It also creates avoidable backend divergence risk: interpreter recursion
uses the host call stack, while compiled code depends on wasm call-stack limits
and no tail-call guarantee.

## Historical code evidence

- `std/iter.witchy:93-104` — `filter_step` calls
  `filter_step(next(rest), keep)` for every rejected element before yielding.
- `std/iter.witchy:106-117` — `filter_map_step` recurses on every `None`.
- `std/iter.witchy:144-155` — `drop_while_step` recurses through the dropped
  prefix.
- `std/iter.witchy:157-165` — `drop` recurses while skipping `k`.
- `std/iter.witchy:322-331` — historically, `find` recursively scanned the
  non-matching prefix.
- `std/iter.witchy:333-355` — historically, `any` / `all` recursively scanned
  until the short-circuit element.
- `std/iter.witchy:357-379` — historically, `last` / `position` recursed per
  consumed element.
- `std/iter.witchy:381-411` — historically, `min` / `max` recursed to exhaustion.
- `std/iter.witchy:210-218`, `:229-282`, and `:292-307` show the local safer
  pattern: `for_each`, `FromIterator` impls, `fold`, `count`, and `sum` drive
  iterators with `while` loops instead of recursion.
- `spec/stdlib.md:807`, `:924`, and `:940` repeat the public safety story for
  lazy/infinite iterators and successful `find`/`position`.

This is distinct from BUG-309. BUG-309 is a docs issue about `for` rejecting
`Iter(a)` and not naming `iter.for_each`; this bug is about `std/iter`'s own
implementation making ordinary bounded iterator programs stack-depth-sensitive.

## Fix direction

Fixed by rewriting the skip paths and consumers to use loop-driven local state,
matching the pattern already used by `fold` and `collect`:

- `filter_step`, `filter_map_step`, and `drop_while_step` now loop until they
  find a yielded element or hit `Empty`.
- `drop` already advances with a `while` loop before returning the remaining
  iterator.
- `flat_map_step` now avoids recursive skipping across empty inner iterators.
- `find`, `any`, `all`, `last`, `position`, `min`, and `max` remain covered so
  their current loop-driven shape does not regress.
- Focused tests cover long rejected prefixes and late matches on both backends.

If recursion is intentionally accepted for some APIs, the generated stdlib docs
should say so explicitly and avoid claiming successful `find`/`position` are
operationally safe on unbounded or very long inputs.
