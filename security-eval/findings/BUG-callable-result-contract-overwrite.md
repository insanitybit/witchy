# Callable result contract was overwritten before validation

Status: fixed on `impl/rfc0165-a1-native`.

## Root cause

Initial SafeWIR checking derived each function body's result contract, then
assigned the derived results into the public signature contract before the
candidate was checked. A declaration claiming `Float` could therefore be
silently replaced by the body's `Int` result and accepted. The declaration was
never compared with the independent body-derived result.

## Security impact

Callable result contracts describe the memory and value authority available to
callers. Silently replacing a declaration with an inferred result can make the
checked ABI differ from the checked source contract. A caller could rely on a
false result layout or ownership classification, weakening the contract boundary
and potentially routing a result through the wrong memory interpretation.

The bug was an acceptance bug in the checker, not a raw encoder fallback. The
existing negative test exposed it because it expected a mismatched `Float`
declaration for an `Int` body to reject, but checking returned `Ok`.

## Fix and evidence

The checker now preserves the declared result contract, derives the body result,
and rejects any nonconformance. A matching `Int` declaration is accepted by a
new positive test.

