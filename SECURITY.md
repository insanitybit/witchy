# Security Policy

If you believe you've found a security issue in witchy — a sandbox escape, a
capability bypass, or anything else — please open a GitHub issue.

We'd prefer reports start out private (GitHub's "Report a vulnerability"
private reporting works) so a fix can land before details spread — but it's
your finding, and how you disclose it is ultimately your decision.

## Capability representation

The compiled backend represents live host authority as opaque WebAssembly
`externref` values. Capability references never enter linear memory and cannot
be forged from integers. Concrete tuples and nominal instances, closure
environments, concrete function signatures, `Option`/`Result`, and
reference-bearing `List` values preserve reference types through typed Wasm GC
structs and arrays.

Boundaries without a concrete reference-aware layout fail at check time. This
currently includes `Dict` keys or values that carry references, an open generic
function ABI instantiated with a capability-bearing value, `region:` copy-out
carrying a capability, and capability-typed callbacks crossing an isolated
worker. The compiler never silently boxes a capability into an integer slot.
