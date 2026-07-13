# BUG-575: Match-arm mutator method silently discards write-back

Status: OPEN
Severity: HIGH
Component: `witchy-types`, method lowering, RFC-0043

## Summary

A statement-form mutator used as a match arm's expression does not write its
result back to the mutable receiver. The call succeeds on both backends, but
the receiver retains its old value:

```witchy
fn collect(items: List(Option(String))) -> Result(List(String), String):
    var out: List(String) = []
    for item in items:
        match item:
            None -> return Err("missing")
            Some(value) -> out.push(value)
    Ok(out)
```

`collect([Some("x")])` returns an empty list on both the interpreter and
compiled WASM; the equivalent functional assignment `Some(value) -> out =
list.push(out, value)` returns `["x"]`. This is silent data loss. It surfaced
while applying RFC-0071 to PM's compiler-footprint decoder: the embedded `pm
audit` command reported an empty capability list after the idiomatic rewrite,
and returned the correct demands when the assignment form was restored.

## Expected

RFC-0043 statement-form mutator semantics apply in discarded match-arm
position, rewriting the arm to an assignment to the original mutable place
before either backend lowers it. A differential regression should cover a
mutator arm nested in a loop, matching PM's accumulator shape.

## Workaround

Use the explicit functional assignment in match arms. The PM site retains that
form with an `idiom-exempt (BUG-575)` comment until lowering preserves the
write-back.
