# Canonical lowering refuses source statements after unconditional fail

Status: open coverage defect; fail-closed, not observed unsafe emission.

While adding Result-coalescing laziness coverage on integration code
`9db4a4136`, the source checker accepted a String-returning default containing:

```witchy
fn forbidden_default() -> String:
    fail("the success edge evaluated its default")
    "unreachable"
```

The source interpreter completed the enclosing test, whose successful Result
never evaluates this default. Canonical compilation nevertheless refused with
`UnsupportedStatement("statement after a terminating call")`.

Evidence: `/tmp/witchy-result-coalesce-laziness.log`; the test reached
`oracle(...)` after its source-output assertion. This is distinct from the
earlier invalid fixture with no String tail, which failed source typing.

The focused coalescing test uses a conditional `fail` to test lazy evaluation
independently of this defect. It does not establish support for unreachable
source statements. The eventual lowering repair must preserve authenticated
proper-tail obligations and divergence rather than fabricate a typed value or
skip final validation.
