# Canonical indirect-call eligibility omitted capability lanes

Status: repaired in the backend and initial checker; focused controls passed
in the 119/119 run `723450bf-309a-45ac-996f-265ef99b1899`.

The captured-Fetch GET/POST probes passed source checking and SafeWIR checking,
then backend lowering refused `VarCall memory-bearing or mismatched signature`.
The indirect-call eligibility helpers omitted existing `Capability` and
`NullableCapability` ABI lanes, including the result of a `fn() -> Fetch`.

The repair retains exact callable signatures, argument/result kinds,
parameter-contract types, Value ownership with no memory effects, and all
reference/GC ownership and proper-tail restrictions. No new memory operation,
authority constructor, encoder instruction or plan semantics is required.

This is a false refusal. Positive ordinary/tail calls, negative mismatched
kind/signature/ownership/effect cases, and the unchanged source probes passed.
This focused evidence is not a compiler-wide memory-safety claim.
