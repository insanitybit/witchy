# Active WIR inliners duplicate effectful arguments

## Status

Remediated for production by sealing both legacy WIR inliners behind
`cfg(test)` and removing their production invocations. Their implementations
remain test-only until equivalent SafeWIR passes have explicit refinement
validation or are deleted during the RFC-0165 cutover.

## Impact

Both `inline_direct_calls` and `inline_recursive_calls` substituted each call
argument expression directly for every use of the corresponding parameter.
They did not first evaluate arguments once and bind their results. A callee
that reads one parameter more than once could therefore cause an effectful or
trapping argument expression to execute multiple times after inlining.

This violates Witchy's call semantics and can change memory, trap, allocation,
or host-observable behavior. The defect is optimizer correctness rather than a
SafeWIR admission bypass: both resulting programs may remain memory-safe while
having different behavior.

## Production reachability

The direct inliner was enabled by `Opt::DirectCall`, which is included in the
default release optimization set. The recursive inliner was called
unconditionally by the legacy WIR optimizer. They were therefore active
production transformations, not dead latent code.

## Cause

The implementations constructed a parameter-to-expression map from cloned
call arguments and recursively replaced `GetLocal` uses with those expressions.
The eligibility checks did not exclude every effectful operand: loads and
`memory.grow` were accepted, and the recursive path did not categorically
exclude all raw memory effects.

## Required replacement

Do not restore either WIR pass. A future SafeWIR inliner must preserve
left-to-right, exactly-once argument evaluation and must obtain a per-instance
semantic refinement result before its candidate is accepted. Proper tail calls
are a mandatory SafeWIR lowering property under RFC-0090 and must not depend on
either inliner.
