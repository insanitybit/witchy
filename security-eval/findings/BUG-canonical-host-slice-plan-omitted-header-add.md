# Canonical HostCall exact plan omitted required structure

## Impact

The mechanical HostCall encoder authenticated the physical byte range as
`object + 4 + start + length`, but the independently constructed exact plan
omitted the addition after the four-byte header constant. The section-order
plan also omitted the mandatory code section when imports and static data were
present. Production admission therefore rejected every canonical host
byte-slice call with `PlanMismatch`. No mismatched bytes were admitted.

## Fix

The plan now includes the same checked header addition as the encoder and the
section plan includes the code section. The
focused `console_print_reaches_admitted_execution` probe exercises checked
source through final SafeWIR validation, mechanical encoding, exact-plan
admission, Wasmtime instantiation, and the physical UTF-8 host slice.
