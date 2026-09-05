# Canonical List(String) push rejects owned UTF-8 storage

Reproducer:

```witchy
fn main(console: Console):
    var xs = ["first"]
    xs.push("second")
    console.print(xs[1])
```

At parent d117afe6a, the existing executable probe fails initial validation
with `UnsupportedMemoryOperation { operation: "layout scalar" }`.
`require_owned_slots_initialized` tries to convert the RawByte storage layout
of a string into a first-class scalar type while checking its release.
Catalog scalar storage contains no owned child slots; this ownership check
must classify storage rather than require a scalar value representation.

The push template also describes pointer MoveRange slices with the containing
list's reference type. Both the lowerer and range checker now use the value
type of the catalog-authenticated owned pointer slot. A negative test rejects
the old container-as-element type. Existing checks still reject uninitialized
source slots, overlapping storage, initialized destinations, and reused source
ownership. The backend continues deriving offsets and strides from the same
validated catalog.

Push now invokes the existing authenticated length/capacity header loader when
its extent bindings are absent. Growth remains conservative fresh allocation:
guard length against capacity and the allocation limit, allocate length + 1,
move the initialized pointer prefix, append the owned element, publish the new
length, and release the emptied source. This is not a claim of general shared
pointer-list mutation support or in-place optimization.

Focused executable command (absolute warmed target):

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-list-mutation-target \
taskpolicy -c utility cargo test \
--config 'profile.dev.package.witchy-lower.opt-level=0' \
-p witchy-lower --lib \
canonical_pipeline::tests::list_string_push_moves_owned_elements_and_executes -- --exact
```

The probe passes initial validation, final admission, Wasm instantiation and
execution, and asserts the printed bytes are `second`.

The pre-existing helper-string dispatch is unchanged. The current TypeTable
has expression and callable types but no checked method-identity witness;
introducing that cross-frontend witness is outside this focused range fix.
No helper-name authority or callable-contract exception was added.
