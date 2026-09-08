# Recursive source shapes require graph-aware layout projection

Status: paired correction tested; not a shipped-code finding.

## Change and failure path

The change to `witchy-types/src/canonical_aggregates.rs` replaces the
one-level nominal-field limit with a cycle-aware source-shape traversal.
Its three focused shape tests pass, including self and mutual recursion,
unsupported fields after a recursive edge, and packed/capability refusal.

That result is insufficient to enable the change in production. Two downstream
consumers still recursively expand those shapes without a visited/active set:

1. `witchy-lower/src/canonical_layouts.rs::LayoutProjection::visit_type`
   visits each non-scalar field before recording the containing layout root.
2. `canonical_layouts.rs::intern_v1_layout` recursively interns a non-scalar
   field's pointee before constructing its owning slot.

For a shape containing itself through an owned field, both traversals can revisit
the same shape indefinitely. The existing opaque-nominal admission refusal is
later in the pipeline and cannot protect either traversal from stack overflow.
This was found by inspecting the consumers, not by executing a crashing compiler
probe. No end-to-end recursive source admission is claimed.

## Preservation and landing boundary

The original incomplete source-shape diff is preserved at
`/private/tmp/rfc0165-recursive-shape-incomplete.patch`. It was excluded from
the queued destructor commit `361cf3555` and the hex-encoding commit
`1b542af63`. The source-shape and layout-traversal changes must land together.

## Required correction

The root collector must traverse a finite type graph without recursively
expanding an already active node. Layout construction must reserve an
authenticated opaque identity before following recursive edges and bind it to
the completed concrete layout exactly once. Inline recursion must still refuse;
merely truncating the traversal or returning an invented layout is not a fix.

The paired change requires a focused production compilation probe demonstrating
termination with either precise fail-closed rejection or admitted execution,
as appropriate to the remaining nullable-slot implementation. Shape-only tests
must not be used as evidence of that pipeline behavior.

## Correction and evidence

The user authorized the paired correction. Root collection now records active
and completed source types, and layout construction rejects a recursive active
type with `requires nullable owned-slot lowering` before expanding it again.
Three focused projection tests pass for self recursion, mutual recursion, and
retained support for deeper acyclic aggregates. Two production-pipeline probes
also pass: valid recursive source reaches the precise layout refusal, and
deeper acyclic records agree across source interpreter, canonical oracle, and
admitted Wasm with no leaked objects.

Commands from the integration worktree, using its existing warmed target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner \
  taskpolicy -c utility cargo test -p witchy-types --lib recursive_shape_tests
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner \
  taskpolicy -c utility cargo test -p witchy-lower --lib recursive_layout_projection
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-inner \
  taskpolicy -c utility cargo test -p witchy --test misc recursive_source
```

Observed results: three source-shape tests, three layout-projection tests, and
two production probes passed. These establish the paired regression correction
and acyclic nesting coverage, not recursive admission.

The eventual nominal-binding construction described above remains necessary for
recursive admission; the early refusal fixes termination without pretending that
nullable source lowering exists. Compiler-wide memory safety remains
unestablished.
