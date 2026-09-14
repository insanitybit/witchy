# Method dispatch lost the result type of a local callable

Status: repaired; focused controls and unchanged source probes passed in the
119/119 run `723450bf-309a-45ac-996f-265ef99b1899`.

A local `fn() -> Fetch` returning a captured record field could be called,
but a subsequent `client.send_raw(...)` failed source checking with
`cannot resolve the method call`. The quiet dispatch pass consulted global
function signatures for a named call without first using a lexical local
callable's declared result. Both dispatch passes need the lexical result.

The repair reads local callable types before global signatures and does not
fall back to a global declaration when a local binding is unknown or not
callable. Ordinary source checking still validates the resulting program.
Tests cover Fetch, Net, a record method, shadowed globals, and preservation
of the Net Connect requirement. The original captured-Fetch three-leg
GET/POST programs remain unchanged.

The observed defect is a false refusal, not an authority grant. Acceptance
still requires the independent SafeWIR checker and exact-byte admission.
