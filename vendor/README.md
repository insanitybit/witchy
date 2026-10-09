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

On that elided path, `compute_scaled_first_addr` reassociates the address.
It flattens the index's `iadd` tree into terms (at most eight) and moves the
constant terms (an `iconst`, or a zero-extended one) into the access's
static offset while it fits 32 bits. It adds the scaled terms (a shifted
zero-extended value) to the heap base first, then the remaining terms. The
address is unchanged, since addition modulo 2^64 is associative. But the
instruction selector folds the offset into the load or store, and
`heap + scaled` becomes a value of its own: shared by every access through
the reference and hoistable out of loops. A field of a large-heap object is
`(uextend(ref) << 3) + 8 + c` (RFC-0179 R4).

A 64-bit memory gets no guard region, so `bound + offset + access_size` must
fit the reservation itself.

The test `memory64_range_elision` (`tests/misc`) pins its behavior.

## cranelift-codegen 0.136.2

This is crates.io `cranelift-codegen` 0.136.2, used through `[patch.crates-io]`.
It carries one change, in the aarch64 backend (RFC-0179 T6). A new
`AluRRRExtendShift` instruction is the ADD/SUB extended-register form with
its imm3 shift (`add xd, xn, wm, uxtw #k`, k 1-4); upstream always emits a
zero shift. `lower.isle` lowers `iadd(x, ishl(uextend(y: i32), k))` to it, and
an address-mode rule in `inst.isle` uses the address value for a base plus
an index scaled by other than the access size. That value is lowered once
and shared by every access through it. A large-heap reference under a
32-bit load is one such index.

The changes sit in `inst.isle`, `lower.isle`, `lower/isle.rs`, `inst/mod.rs`
and `inst/emit.rs`, each marked `witchy (RFC-0179 T6)`.
`tests/misc/aarch64_uxtw_shift.rs` pins the value and the fold.

Neither patch is sent upstream. When Wasmtime is upgraded, re-vendor the
matching versions and re-apply both.
