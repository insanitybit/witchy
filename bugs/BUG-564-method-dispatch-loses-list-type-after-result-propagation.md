# BUG-564: method dispatch loses a list type after `Result` propagation

- Status: FIXED
- Severity: MEDIUM
- Area: type checker / RFC-0046 trait and method dispatch
- Found: 2026-07-09 during the RFC-0071 Coven modernization

## Symptom

The canonical method spelling resolves on a directly typed list, but the same
call is rejected when the list came through `?` from a `Result`:

```witchy
import result

fn ids() -> Result(List(String), String):
    Ok(["a"])

fn has() -> Result(Bool, String):
    let xs = ids()?
    Ok(xs.contains("a"))

fn main(console: Console):
    console.print("${has() ?? false}")
```

`witchy check` reports:

```text
cannot resolve the method call `.contains(…)` — methods come from `impl` blocks;
a plain function is called as `contains(value, …)`
```

Replacing only the failing expression with `list.contains(xs, "a")` checks and
passes `witchy parity`. A direct `let xs = ["a"]` followed by
`xs.contains("a")` also checks and passes parity.

## Impact

RFC-0071 makes method form the repository's canonical spelling, while RFC-0054
increases the number of values obtained through `Result` propagation. Those two
directions currently conflict in ordinary generic code. Coven's maintainer-list
authorization needs a documented module-form exemption at two call sites.

## Likely cause

The annotate/monomorphize fixpoint retains the `Result(List(String), E)` call
signature well enough to type `xs`, but method-origin resolution does not recover
that concrete receiver type after the `?` expression. This is the remaining
RFC-0046 generic-chain class, not a missing `List.contains` implementation.

## Done when

- The reproducer checks and runs with `xs.contains("a")` on both backends.
- Method lookup uses the checker-owned type of a value bound from `e?`.
- Coven's BUG-564 module-form exemptions return to method form.
- A regression test covers a generic std method after `Result` propagation.

## Resolution

The empty-table quiet pass now derives the payload type of `Expr::Try` from the
operand's declared `Option` or `Result` type. This seeds a `let x = e?` binding
with the same concrete receiver type the checker later verifies, allowing the
normal owner-method lookup to resolve without a per-method special case.

Crate regressions cover both `Result(List(String), E)?` and
`Option(List(String))?`, and Coven's maintainer checks use canonical
`maintainers.contains(id)` spelling again.
