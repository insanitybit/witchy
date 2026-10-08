# Vendored crates

## wasmtime-internal-cranelift 49.0.2

This is crates.io `wasmtime-internal-cranelift` 49.0.2, used through
`[patch.crates-io]` in the root `Cargo.toml`. It carries one change, in
`src/bounds_checks.rs`: range-based bounds-check elision (RFC-0179, R2).

Wasmtime elides the explicit check on a 32-bit index because the index's
*type* keeps it below 4 GiB, so every address falls inside the reservation
plus guard region. The patch applies the same rule to the index's *value*.
`index_upper_bound` bounds it through `uextend`, `iconst`, `iadd`, `ishl` by
a constant and `band` with a constant. When `bound + offset + access_size`
fits inside the reservation plus guard region of a memory that never moves,
no check and no Spectre guard is emitted. An access is then either in bounds
or faults, so out-of-range accesses still trap.

The patch is carried here and is not sent upstream. When Wasmtime is
upgraded, re-vendor the matching version and re-apply the change; the test
`memory64_range_elision` in `crates/witchy-runtime/tests/runtime` pins its
behavior.
