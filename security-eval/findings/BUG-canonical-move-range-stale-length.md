# Canonical `MoveRange` left a stale initialized length

## Status

Fixed on `impl/rfc0165-canonical-v1`.

## Impact

The canonical list-growth lowering moved owned pointer slots into a replacement
allocation and zeroed the source slots, but left the source list's logical
length unchanged before `DropRef`. A layout-directed destructor would therefore
walk the old initialized prefix and encounter slots whose ownership had already
been transferred. Treating those zero slots as nullable would hide the broken
ownership invariant and make malformed owned storage admissible.

## Fix

Pointer-bearing list growth now emits a checked `UpdateCount(old_object, 0)`
immediately after `MoveRange` and before `DropRef`. The SafeWIR checker permits
that transition only when no stored object or nonempty initialized range remains
for the source. Recursive destruction rejects a zero pointer in any initialized
owned slot.

## Evidence

```sh
env RUST_WRAPPER= RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-canonical-target-b \
  cargo test -p witchy-lower \
  list_string_push_moves_owned_elements_and_executes -q
```

The focused production-pipeline probe passes and executes after independent
final SafeWIR validation and exact-plan admission.
