# Canonical lowering duplicated callable result authority derivation

## Finding

Canonical source lowering ran a CFG-incomplete, local origin analysis and wrote `Fresh`,
`StaticData`, or `Shared` result contracts into function signatures before independent SafeWIR
validation. The checker later performed its own body-derived analysis, but retaining the lowerer
analysis created a second authority-shaped implementation and allowed future callers to mistake an
untrusted lowering claim for a checked contract.

## Resolution

The lowerer-side fixed point and origin classifier are deleted. Lowering emits an untrusted
placeholder required by the candidate representation; `check_initial` derives result contracts
from complete function bodies and replaces the placeholder before any `InitialValid` authority is
minted. Final and transformed checks continue to require exact contract agreement.

The focused static-string result probe reaches exact-byte admission with the lowerer inference
absent, demonstrating that the production path consumes checker-derived `StaticData` authority.

## Evidence

- `checked_utf8_string_literal_reaches_admitted_static_data_pointer`
