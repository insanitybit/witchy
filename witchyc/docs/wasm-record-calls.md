# Numeric helpers in record-proved code

```
// The first byte of a scalar's UTF-8 encoding.
fn utf8_lead(scalar: U64) -> U64:
    requires scalar <= 1114111
    ensures result < 256
    proof:
        auto
    ...

    fn push_scalar(input: String, allocator: &Allocator, scalar: U64) -> Result<String, String>:
        ...
        match utf8_push(allocator, move vector, U8.truncate(utf8_lead(scalar)), keep):
```

A function proved by the record model (library code that reads a sealed
record's fields, such as `String.push_scalar`) may now call a function whose
contract is numeric only. Before, the record model stated any such call as a
path that is never taken, so it could not be written and library bodies kept
their arithmetic inline.

## The rule (`src/compiler/record_memory.rs`, `scalar_called`)

For a nongeneric callee with a numeric contract, unsigned parameters, and an
unsigned or Bool result:

1. each `requires` clause, with the parameters replaced by the actual
   arguments, is shown at the call, on the path that reaches it;
2. the answer is a new value (bounded by its type, like any other);
3. each `ensures` clause, about that value and the arguments, is assumed on the
   path after the call.

A clause the rule cannot render (fields, methods, `old`, I64 integers, a Bool
read as a number, a clause for one variant of a `Result`) leaves the call as it
was: stated as never reached, so the proof fails rather than assuming anything.

The callee's own body is proved by the numeric walk (`src/compiler/scalar_vc.rs`),
which relies only on numeric contracts. A numeric function cannot be proved
from a record contract, so no proof can lean on itself through this rule.

The numeric walk no longer states call-site goals for a function with a record
contract: the record model checks each of those calls itself, with the
caller's own record `requires` in view, which the numeric walk could not see.
(Before this change that walk was the one that stated them; it now skips such
functions.)

## What it bought

`String.push_scalar` branched four ways on the scalar's size and twice more in
its loop; the record model stated each path, and it was the largest single
proof statement in the library, about 2 ms to write out on every build. The
byte arithmetic is now in three helpers with numeric contracts (`utf8_following`,
`utf8_lead`, `utf8_continuation`), and `push_scalar` has one path. Cached builds
of `hello`, `words`, `anagrams` and `ledger` are 0.5 to 1.5 ms faster.

## Fixtures

`tests/fixtures/record-calls/`: `numeric-helper.wy` runs; `requires-unmet.refused.wy`
(the caller cannot show the helper's precondition) and `claims-more.refused.wy`
(the caller claims more than the helper gives) are refused by Lean.

## Trusted

The rule is part of the record model, which is trusted: it turns a body into
Lean statements, and a mistake in it could state too weak a goal. It renders a
numeric clause into the same Nat arithmetic the numeric walk proves it in
(`-` stops at zero, `/` by zero is zero), with the actual arguments for the
parameters.
