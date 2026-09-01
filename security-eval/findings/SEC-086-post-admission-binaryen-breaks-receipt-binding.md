# SEC-086: Post-admission Binaryen rewrote receipt-bound Wasm

## Confirmed bug

The production runtime accepted compiler-emitted/admitted Wasm bytes, then
ran Binaryen/`wasm-opt` and cached or compiled the transformed bytes. The bytes
that reached compilation could therefore differ from the exact bytes admitted
by the compiler, invalidating exact-byte admission and receipt binding.

**Reachability:** production runtime path for admitted compiler artifacts.
**Severity:** High. This broke the byte-identity security boundary rather than
merely changing an optimization result.

## Remediation

Fixed on `impl/rfc0162-0163-unified` by `96156a95d`: admitted bytes are compiled
exactly, with the post-admission Binaryen rewrite/cache path removed.

