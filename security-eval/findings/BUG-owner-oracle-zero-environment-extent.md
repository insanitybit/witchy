# Closure oracle recorded a zero-byte environment

Status: fixed by deriving the fixed extent from the admitted canonical layout.
Observed on the Unit integration based on `ef338976c`, 2026-09-10.

`canonical_interpreter::execute_owner_operation` allocated every opaque
closure's linear environment with `RuntimeExtent::FixedBytes(0)`. Wasm instead
allocates the descriptor's fixed byte size. Existing scalar cell reads did not
expose this difference; the new Unit projection check correctly refused a
field access beyond the oracle's recorded zero-byte extent.

The focused `rfc0165_oracle::unit_capture_with_capability_three_legs` program
passes `Nil` and `Console` into a function that returns a closure, then calls
that closure. Source execution prints `tick` and `7`. After adding Unit to the
private bridge's supported scalar storage, the oracle refused with
`TypeMismatch("Unit field layout")` because its extent was zero.

The correction resolves the environment's canonical nominal layout, requires
its descriptor to have a fixed size, and records that exact size. No projection
check or memory-safety rule is weakened. This was a false oracle rejection,
not evidence of an unsafe admitted Wasm access.

Focused regression command:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-reflect-specialization-target CARGO_BUILD_JOBS=2 taskpolicy -c utility cargo test -p witchy --features witchy-wir/native --test misc rfc0165_oracle::unit_ -- --nocapture
```
