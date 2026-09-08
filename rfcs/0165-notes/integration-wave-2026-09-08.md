# SafeWIR integration wave: 2026-09-08

## Delivered in this branch, pending queue validation

- `361cf3555`: bounded-stack owned graph release, with child-before-parent
  traversal, checked temporary-frame growth, and cleared temporary storage.
- `1b542af63`: real standard-library hex encoding and exact checked-function
  reachability for monomorphized callable obligations.
- `f34b49a1d`: ClosureSlot is explicitly a Load/Store subjudgment, never its
  own instruction receipt. This corrects the narrow B7 manifest finding, not
  the entire assurance ledger.
- `da2c4f7fb`: base64 encoding with a pre-multiply output-size bound, preserving
  the runtime access guards. The independent review finding and correction are
  recorded in `base64-review-2026-09-08.md`.
- `aa1539c56`: finite nominal source-shape traversal, deeper acyclic aggregate
  support, and explicit rejection before recursive layout expansion. The
  source-shape review is recorded separately.
- `ac81acb12`: generic reachability regression pin, retaining missing-body
  refusal and reachable specialization authentication.
- `84c3f8fe0`: refcount-boundary fixture uses the production scratch requirement.
- `9c7030dbc`: nullable owned-slot descriptor transport, sealed at initial
  validation across slot, pointer-buffer, and descriptor-field forms.

## Focused evidence

Use the existing absolute warmed integration target, with
`CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner`, clear both
`RUSTC_WRAPPER` and `CARGO_BUILD_RUSTC_WRAPPER`, and run builds at utility
priority. No local full gate was run.

| Probe | Observed result |
|---|---|
| `cargo test -p witchy --test misc rfc0165_encoding` | 3 passed; hex/base64 source interpreter, canonical oracle, and admitted Wasm agree; reachable unsupported decode refuses |
| `cargo test -p witchy --test misc recursive_source` | 2 passed; deeper acyclic records execute without leaks, recursive source reaches the precise refusal |
| `cargo test -p witchy-types --lib recursive_shape_tests` | 3 passed |
| `cargo test -p witchy-lower --lib recursive_layout_projection` | 3 passed |
| `cargo test -p witchy-wir --lib --features native refcount` | 4 passed, including unchanged underflow/overflow attack assertions |
| `cargo test -p witchy-wir --lib --features native nullable_owned` | 2 passed; one full initial-validation refusal and an additional pointer-buffer predicate check |

Worker-side focused evidence also covers the 25,000-node destruction kernel,
wide and shared-edge graphs, nullable transport round-trip, all nine manifest
tests, five generic-witness pins, and the base64 output-size boundary. Consult
the corresponding review/evidence notes for their limits.

The previous queued destructor tip failed only its refcount-underflow fixture:
it still supplied five scratch locals instead of the new twelve. The other
reported failures were cancellation. The fixture correction preserves the
attack and its no-write assertion; the named tests above now pass.

## Remaining work

Nullable storage operations (`InitNullableOwned`, checked `ClonePresent`) are
not implemented. Neither nullable nor recursive nominal values are admitted.
The next dependency is complete checker/oracle/backend support for that operation
family, followed by checked-source nominal bindings and recursive lowering.
Generic nominal instances and decoding kernels remain coverage work. The B1
RFC-to-rule crosswalk, broader language families, and optimization assurance
remain open. No fresh corpus count or compiler-wide soundness claim is made.
