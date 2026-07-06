---
rfc: 0064
title: "Complete the mutation classification: enforce row 3, reject ambiguous inference, extend the discard error to free calls"
status: implemented
created: 2026-07-05
predecessors:
  - "0043 (declared mutation — defines the classification this RFC finishes enforcing)"
  - "bugs/BUG-209, BUG-213, BUG-242 (the three shipped gaps, each independently probed)"
tracking:
  - "Check 1 (row 3): `check_var_conventions` in crates/witchy-types/src/typeck.rs"
  - "Check 2 (ambiguous inference) + Check 1's elided-tail completion: the post-inference block in `check_function`, same file"
  - "Check 3 (discard on free calls): `rewrite_block`'s bare-expression arm in crates/witchy-types/src/traits.rs"
  - "Tests: `rfc0064_*` in crates/witchy-types/src/typeck_tests.rs (unit) and src/example_tests.rs (differential, both backends)"
---

# RFC-0064: Complete the mutation classification

> Code blocks are deliberately **not** tagged `witchy` so the doc-examples
> sweep does not execute pre-implementation snippets.

## Summary

RFC-0043 (implemented) made "which statements mutate" a *declared* property:
a `var` parameter is either a **procedure channel** (returns `Nil`) or a
**mutator receiver** (first param, self-typed return), every other `var`
shape is a compile error, and a discarded non-mutator statement is loud.
Three pieces of that contract never shipped, and together they reconstruct
the exact silent-failure family the RFC was written to kill:

1. **The row-3 shape error is missing** (BUG-242). `var` in a non-first
   position with a self-typed return compiles with the abolished *combined*
   semantics (writes back AND returns); `var` first with an unrelated return
   type checks, runs on the interpreter with combined semantics, and is
   rejected by the WASM backend — an accidental interpreter-only shape.

2. **Classification is inferred when the return type is elided** (BUG-213).
   `fn bump(var xs: List(Int), by: Int): xs.push(by)` — the natural spelling
   of a procedure — silently classifies as a *mutator* because the inferred
   tail type is `List(Int)`. Every free call becomes a no-op with no
   diagnostic. One indentation-level line flips call-site semantics.

3. **The discard error covers method statements only** (BUG-209).
   `list.push(xs, 2)` as a bare statement — the call form of a documented
   mutator, which rfcs/0043:192-195 promises "fall[s] under the discard
   error" — checks clean and does nothing. Same for user mutators called in
   free form and for any discarded non-Nil free call.

This RFC finishes the cut with three checks in one place, plus one new rule
(the reason this is an RFC and not just bug fixes): **a `var` first
parameter with an *elided* return type must annotate** when its inferred
return type would make it a mutator. Everything else is enforcement of
decisions RFC-0043 already made.

## Motivation (all probed 2026-07-05, HEAD ~d808d42, both backends)

The three gaps compose into a gauntlet for the most ordinary task in the
language — "write a helper that pushes to my list":

```
fn bump(var xs: List(Int), by: Int):      # natural spelling
    xs.push(by)

fn main(console: Console):
    var xs: List(Int) = []
    bump(xs, 5)                           # checks clean, prints [] — silent no-op
    print(console, "${xs}")
```

- The natural spelling is a silent no-op (gap 2: inferred mutator; gap 3: no
  discard error on the free-call statement to flag it).
- The correct spelling, `-> Nil` with the same body, is a type error (the
  tail is `List(Int)`), pushing the user to pad the body.
- The obvious pad, a bare `Nil` tail, is checker-accepted but WASM-rejected
  (BUG-214, separate).
- The pads that work — trailing `return` or `let _ = 0` — are documented
  nowhere.

Meanwhile the shape the RFC-0043 breaking-change note says "gets the row-3
compile error" (`push_twice`-style combined channels, just with the `var`
param moved out of first position) still compiles with combined semantics on
both backends (gap 1), so the "no ambiguous case exists" claim in
rfcs/0043:142 and the two-shape contract in spec/language.md:426 are both
false as shipped.

## Design

All three checks live at the same layer: signature/statement validation on
the single linked AST, before either backend lowers — parity by
construction, matching where RFC-0043's shipped pieces already sit.

### 1. Enforce row 3 (pure enforcement, no new decision)

A function with any `var` parameter that is neither `is_var_procedure`
(ast.rs:219) nor `is_mutator` (ast.rs:202) is a compile error, with the
RFC-0043 text verbatim:

```
a `var` parameter must be a write-back channel (return `Nil`) or a mutator
receiver (first parameter, returning its type); split the function or
return a tuple
```

This kills both probed row-3 shapes (BUG-242's t_row3a/t_row3b), including
the interpreter-only one — the program is rejected before any backend runs.

### 2. Ambiguous inference must annotate (the new rule)

If a function has a `var` FIRST parameter and an **elided** return type
whose *inferred* tail type equals that parameter's type, that is a compile
error demanding the annotation:

```
`bump` has a `var` receiver and its body's tail is the receiver's type —
annotate the intent: `-> List(Int)` declares a mutator (statement form
writes back); `-> Nil` (or add `return`) declares a procedure
```

Rationale: RFC-0043's thesis is that write-back is *declared*, "not
inferred" (its own title). Classification-by-inferred-tail-type violates
that thesis for exactly the one signature property that changes call-site
semantics. Requiring the annotation makes the declaration real. An
*explicit* self-typed return still declares a mutator with no extra
ceremony — only the elided-and-would-be-mutator case must choose.

(Non-first `var` params never reach this rule: check 1 already rejected
them. A `var` first param whose inferred tail is any *other* type is a
procedure only if that tail is `Nil`; otherwise it is row 3 → check 1.)

### 3. Extend the discard error to free-call statements (pure enforcement)

A free call in statement position whose declared return type is non-`Nil`
is the same discard error method statements already raise, with the
RFC-0043-promised special case: when the callee is a mutator, name the
method form as the fix —

```
result of `push` is discarded — `list.push(xs, 2)` does not write back;
use the method statement `xs.push(2)`, reassign (`xs = list.push(xs, 2)`),
or discard explicitly (`let _ = …`)
```

`let _ =` stays the escape hatch. `main`'s tail and expression-position
calls are untouched; this is statement position only, exactly mirroring the
shipped method-statement rule (typeck's existing discard machinery keyed on
`Expr::Call` as well as `Expr::MethodCall`).

### Migration

Break-don't-deprecate, one cut:

- std has zero row-3 shapes (RFC-0043's grep still holds) and zero
  elided-return `var`-first functions with self-typed tails (std mutators
  are all explicitly annotated) — expected fallout: none.
- `examples/` + `projects/` + `book/` fences: sweep with the new binary;
  every hit is either a genuine latent no-op bug (fix the call), a missing
  annotation (add `-> Nil` / self-type), or a legitimately-discarded call
  (add `let _ =`). Each fix is mechanical and self-explaining from the
  error text.
- Differential tests: the three rows × {check-error, runtime-behavior};
  both row-3 shapes reject identically on both backends; the BUG-213 trap
  program now errors at the *declaration*; `list.push(xs, 2)` statement and
  a user-mutator free statement both raise the discard error; `let _ =`
  passes; early-`return`/`?` write-back in procedures unchanged
  (spec:426's "even on early return" promise gets a pin while we are here).

## Alternatives

- **Fix the three bugs independently, no annotation rule.** Checks 1 and 3
  are strictly owed; but without check 2 the BUG-213 trap survives in a
  weaker form — the natural spelling would then hit check 3's discard error
  at *call sites* (better than silence), yet the declaration itself remains
  a mutator the author never chose, and a caller who binds the result gets
  combined-looking behavior with no writeback. The declaration-site error
  is earlier, singular, and names the fix. Rejected.
- **Default the elided case to procedure instead of erroring.** Reads
  nicely, but makes `fn f(var xs: List(Int)): xs` (tail = receiver) a
  procedure while the visually-identical explicit `-> List(Int)` is a
  mutator — inference silently choosing the *other* row is the same trap
  mirrored. Explicit over inferred for semantics-bearing signatures.
  Rejected.
- **A statement-form-only writeback for free calls to mutators** (make
  `list.push(xs, 2)` write back like `xs.push(2)`). RFC-0043 already
  rejected this: "the receiver of a method call is syntactically the
  target; the first argument of a free call is not" (rfcs/0043:193). Not
  reopened.

## Drawbacks

- One more parse/annotate-time error class; the annotation rule adds
  ceremony to a signature users may have wanted to leave inferred. The
  ceremony is one token (`-> Nil`) and buys away a silent no-op.
- The free-call discard error will flag currently-compiling
  (silently-broken or intentionally-discarding) code; that is the point,
  but the sweep cost lands on examples/projects.

## Prior art

- Rust: `#[must_use]` + unused-result lints made discarded results loud;
  witchy's version is stronger (error, not lint) per RFC-0043.
- Swift/Hylo: `mutating func` / `inout` — mutation is declared, never
  inferred from body shape; check 2 restores exactly that property.
