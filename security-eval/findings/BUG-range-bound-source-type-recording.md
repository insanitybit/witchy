# Range bounds lost retained-source type facts

Status: corrected; focused source/admission/native/oracle regressions pass.
This was a fail-closed compilation defect, not an observed safety bypass.

`Checker::infer_inner` inferred Range through a cloned, desugared block with
source-address type recording disabled. Parameterized operands such as `n + 1`
therefore had no facts in the retained CheckedModule table. Constant folding
could hide this for a local constant; Coven's parameterized loop exposed it.

```witchy
fn count(console: Console, n: Int):
    for i in 0..n + 1:
        console.print("${i}")
fn main(console: Console):
    count(console, 2)
```

Before correction, canonical layout projection refused with
`MissingCheckedExpressionType` for `Binary { op: Add, lhs: Var("n"), rhs: Int(1) }`.
The correction infers the original operands once, in order, then applies the
same desugared constraints using private typed locals. Temporary AST addresses
remain excluded. No layout inference fallback or validator relaxation was added.

`tests/misc/rfc0165_range_bounds.rs` checks retained bound/child type facts,
parameterized half-open and inclusive ranges, mutation-visible bound ordering
and exactly-once evaluation, empty ranges, exact native/oracle output, and
successful reclamation. All four tests passed within the 55-test focused run
`e4bc1ec0-ee2c-4dfd-9732-1e1e00afe43e`; log
`/private/tmp/witchy-registry-hosts-final-focused.log`. Bounded independent
review found no blocking issue. The package workflow then advanced to its
separate missing atomic Dir storage operations.
