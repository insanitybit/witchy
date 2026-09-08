# RFC-0165 bounded drop worklist review (read-only)

## Verdict

No concrete finding in the reviewed worklist slice.

## Checked invariants

- The new frame is `(object, catalog-row, next-child)`. A parent frame remains present while exactly one child frame is processed, then resumes at the next slot. This preserves child-before-parent reclamation and makes temporary storage proportional to graph depth, not collection width (`crates/witchy-wir/src/canonical_wasmir/drop_worklist.rs:1-6`, `:150-189`).
- Each edge is loaded, checked nonzero, cleared, and pushed independently. There is no visited-set deduplication, so repeated shared edges each consume one reference; a zero count traps before decrement (`crates/witchy-wir/src/canonical_wasmir/drop_worklist.rs:131-186`). Static pointers pop without decrementing, and shared objects decrement then pop without traversal (`:114-142`).
- Frame push checks `top + 12` before writes, calculates required pages in widened arithmetic, traps on `memory.grow` failure, and publishes only the temporary high-water mark through bump global 0 (`:72-91`). Reclamation continues to use free-list global 1 only (`:93-100`).
- After the traversal reaches its saved base, the implementation clears `[base, high_water)` with `memory.fill` before restoring global 0. Therefore a later bump allocation cannot observe residual frame words as supposedly virgin zeroed storage (`:190-193`).
- The expected-plan generator is separately authored and does not decode encoder output. It mirrors frame push/pop/reclaim and cleanup with `PlanToken`, including explicit `LocalTee` and `MemoryFill` decoding support in the parent (`crates/witchy-wir/src/canonical_wasmir/drop_worklist.rs:201-322`; `crates/witchy-wir/src/canonical_wasmir.rs:3896-4008`).

## Coverage and boundary

The three private kernel tests exercise 25,000-depth growth and cleanup, wide graph bounded-frame behavior, and duplicate-edge/static/underflow behavior (`crates/witchy-wir/src/canonical_wasmir/drop_worklist.rs:385-458`). The focused admitted oracle/Wasm test covers nested/shared list behavior and leak auditing (`tests/misc/rfc0165_drop_worklist.rs:6-26`).

The private kernel fixture's `OpaqueNominal` is a mechanical recursive descriptor used solely to test backend traversal. Its own comment states this is not production recursive admission (`crates/witchy-wir/src/canonical_wasmir/drop_worklist.rs:328-338`). This review makes no recursive source-support claim; the existing production opaque-nominal refusal remains the relevant seal.
