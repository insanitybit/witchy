# Bounded-stack owned-reference release

## Scope

Replace recursive backend destructor expansion with an explicit depth-first
worklist. This is a prerequisite for recursive source values, not evidence that
recursive source lowering is enabled. Opaque nominal layouts remain refused by
production admission until their remaining checker and lowering rules land.

Each frame records an object, its layout-dispatch row, and its next child.
Children release before their parent. Shared edges are counted individually,
not deduplicated. Static objects require no count-header write. Temporary frames
occupy checked memory above the saved bump frontier; successful completion clears
that temporary region and restores the frontier.

## Focused evidence

Run from the `rfc0165-drop-worklist` worktree with its existing private warmed
target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test -p witchy-wir --features native \
  --lib drop_worklist -- --nocapture

env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test -p witchy --test misc \
  drop_worklist -- --nocapture
```

Observed: three kernel tests passed and one admitted-launch test passed.

- A 25,000-node graph releases with a bounded Wasm call stack, restores the bump
  frontier, clears worklist memory, and preserves child-before-parent free order.
- A wide graph does not require one simultaneously live frame per element.
- Repeated shared edges decrement independently; reference-count underflow traps.
- Nested and shared owned lists agree across the source interpreter, canonical
  oracle, and admitted Wasm, including allocation after release and leak checks.

The recursive graph kernel fixtures construct test-only backend modules. They
do not pass through checked-source recursive-type admission and must never be
reported as that coverage. The nested-list launch probe exercises the production
pipeline.

## Remaining obligations

Independent destructive-operation review found no concrete issue in the bounded
slice; see `drop-worklist-review-2026-09-08.md` for its scope and reasoning.
This slice has not yet landed through the merge queue. Worklist growth may itself exhaust memory; the
resource behavior needs to remain explicit when recursive source coverage lands.
Nullable owned slots and authenticated recursive source-layout bindings remain
separate prerequisites. Compiler-wide memory safety is not established.
