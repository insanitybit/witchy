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

On that elided path, `peel_constant_addends` also moves a constant addend of
the index (`x + c`, with `c` an `iconst` or a zero-extended one) into the
access's static offset. The bound already proves `x + c` cannot wrap, so the
address is unchanged, and the instruction selector folds the offset into the
load or store while `heap + x` is shared by every field access through `x`
(RFC-0179 R4: a field of a large-heap object is `(ref << 3) + c`).

A 64-bit memory gets no guard region, so `bound + offset + access_size` must
fit the reservation itself.

On the same path, `compute_scaled_first_addr` forms an index `scaled + rest`,
with `scaled` a shifted zero-extended value (a large-heap reference,
`uextend(ref) << 3`), as `(heap_base + scaled) + rest`. The address is the
same, since the bound proves nothing wraps. But `heap_base + scaled` becomes
a value of its own, shared by every access through the reference and
hoistable out of a loop that indexes it.

The test `memory64_range_elision` (`tests/misc`) pins its behavior.

## cranelift-codegen 0.136.2

This is crates.io `cranelift-codegen` 0.136.2, used through `[patch.crates-io]`.
It carries one change, in the aarch64 backend (RFC-0179 T6). A new
`AluRRRExtendShift` instruction is the ADD/SUB extended-register form with
its imm3 shift (`add xd, xn, wm, uxtw #k`, k 1-4); upstream always emits a
zero shift. `lower.isle` lowers `iadd(x, ishl(uextend(y: i32), k))` to it, and
an address-mode rule in `inst.isle` uses it for a base plus an index scaled
by other than the access size. A large-heap reference under a 32-bit load is
one such index.

The changes sit in `inst.isle`, `lower.isle`, `lower/isle.rs`, `inst/mod.rs`
and `inst/emit.rs`, each marked `witchy (RFC-0179 T6)`.
`tests/misc/aarch64_uxtw_shift.rs` pins the value and the fold.

Neither patch is sent upstream. When Wasmtime is upgraded, re-vendor the
matching versions and re-apply both.
