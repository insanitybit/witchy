# Canonical closure signature layout collision

The typed-capture migration gave capture-free closures the same physical
environment and closure layout even when their function signatures differed.
The layout-keyed callable registry then substituted one signature for another.
Production remained fail-closed: the existing scalar lambda launch fixture
failed with `CallArityMismatch { callee: "<closure>", expected: 1, actual: 2 }`.

The repair assigns each environment a domain-separated nominal identity
derived from its normalized checked function type, bound to the physical
environment in the authenticated layout bundle. Allocation and authority
checking resolve that existing nominal binding explicitly. No padding,
unchecked cast, or validator exception is introduced.

Focused evidence: the existing
`admitted_capture_free_scalar_lambdas_are_function_values_bound_to_their_witness`
test passes with unary and binary closure signatures in one program. Full
integration and queue validation are still required; this is not a
compiler-wide memory-safety claim.
