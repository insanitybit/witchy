# SEC-088: Annotated Float functions are rejected before lowering

## Confirmed bug

The focused SafeWIR production test exposed a front-end defect: a normal
annotated function such as `fn float_ops(left: Float, right: Float) -> Float`
is rejected before lowering with `unknown or imported nominal type Float`.
`Float` is a supported scalar form in the language and lowering, so this is a
front-end type-resolution failure, not a SafeWIR validation failure.

**Reachability:** any checked compilation using an ordinary `Float` parameter
or result annotation. **Severity:** Medium availability/correctness impact;
affected programs cannot compile, but the defect does not by itself grant
memory or host authority.

## Current status

Open on `impl/rfc0162-0163-unified`; no existing fix for this front-end
rejection was found on the branch.

