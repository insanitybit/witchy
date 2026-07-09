# BUG-337: spec/language.md:144 over-claims — enclosing-scope assignment to a closure-captured variable is spec'd as a check-time error but is (correctly) accepted

Severity: LOW
Status: FIXED
Verified: 2026-07-08 DOC on master a8cc2cc
Component: spec/language.md, closures, capture semantics, docs

## Problem

`spec/language.md` now scopes the closure assignment rule to assignment inside
the closure body. Reassigning an enclosing-scope `var` after a closure has
captured its old value remains legal; assigning to a `let` or writing to a
captured variable from inside the closure remains a check-time error.

## Historical Problem

`spec/language.md:144-145` as written: "Assigning to a `let`, or to a variable
captured by a closure, is a check-time error (closures capture **by value**;
return the new value or use `var`)." The plain reading promises a check-time
error for an enclosing-scope write to a captured variable that does not occur.

`var x = 1; let f = fn(n: Int): n + x; x = 2; print "${f(10)} ${x}"` passes
`witchy check` and runs "11 2" (both backends agree). The closure keeps its
by-value snapshot (`f(10)=11` uses `x=1`) while `x` itself becomes 2. The
implemented behavior is consistent and sane on both backends — the sentence
over-claims. `spec/language.md:464-465` already words the rule correctly
("Closures cannot assign to captured variables", i.e. inside the closure body).

Controls: assignment to the captured variable INSIDE the closure body correctly
errors ("a closure cannot assign to the captured variable `count` … return the
new value or use a `var` parameter"), and the first half of the sentence
(assigning to a `let`) correctly errors. Only the "variable captured by a
closure" clause diverges from the sentence as written; the fix is a spec wording
tweak, not a checker change. LOW.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-spec/t_assign_after_capture.witchy && $W scratch/ultra-spec/t_assign_after_capture.witchy
t_assign_after_capture.witchy: ok
11 2                                     # both backends agree, parity outcome=agree

# controls: t_closure_capture_assign.witchy (in-closure assign → correct error);
#   t_let_reassign.witchy (let reassign → "it is immutable (declared with `let`)")
```

Probe: `/Users/cobrien/workspace/witchy/scratch/ultra-spec/t_assign_after_capture.witchy`;
controls `t_closure_capture_assign.witchy`, `t_let_reassign.witchy`.

## Code evidence

- `spec/language.md:144-145` — the over-claiming sentence.
- `spec/language.md:464-465` — the correct wording ("Closures cannot assign to
  captured variables", scoped to the closure body).
- Control 1: `t_closure_capture_assign.witchy` — in-closure assignment to a
  captured var correctly errors ("the write would be lost").
- Control 2: `t_let_reassign.witchy` — assigning to a `let` correctly errors.
- No RFC documents an enclosing-scope ban.

## Fix direction

Reword `spec/language.md:144-145` to scope the closure clause to assignment
*inside* the closure body, matching `:464-465`: e.g. "Assigning to a `let` is a
check-time error; a closure cannot assign to a variable it captured (captures are
by value — return the new value or use a `var` parameter)." Enclosing-scope
reassignment of a captured `var` after the closure is defined stays legal. No
code change.
