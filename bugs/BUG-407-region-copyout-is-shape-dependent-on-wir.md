# BUG-407: `region:` copy-out is shape-dependent on the WIR backend

Severity: MED
Status: FIXED
Fixed: 2026-07-09 — WIR region copy-out now covers records, generic records, ADTs, recursive ADTs, and Dicts in addition to String/List/Tuple/scalars.
Verified fixed: exact copy counters cover recursive `Stack(Int)` (44 bytes) and `Dict(String, List(Int))` with runtime-built nested values (50 bytes); existing record/shape parity regressions remain green.
Verified: 2026-07-06 CODE on master 7bb3ee7
Component: `region:`, WIR lowering, performance modes, RFC-0016/RFC-regions

## Problem

The public `region:` contract says pointer-valued region results escape by
deep-copying the block value to the entry watermark, then reclaiming the region's
temporary allocations. The implemented WIR path only has region copy-out helpers
for `String`, `List`, and `Tuple` shapes. Region blocks whose result shape is a
record, ADT, recursive ADT, or `Dict` silently fall back to lowering as a plain
block: correct value, but no reclaim.

That makes a shipped performance feature depend on result shape in a way the
language docs and RFC do not expose. A user can wrap a hot allocation block in
`region:` and still get ordinary heap growth if the result is a record or enum,
even though the same feature works for scalar/list/string-style examples.

## Evidence

- `rfcs/regions.md:28-34` defines region escape as deep copy-out followed by
  heap reset, with parent-side short-circuiting.
- `rfcs/regions.md:87-101` lists copy-out support for `String`, `List`,
  `Tuple`, `Record`, `ADT`, and `Dict`, and says an unresolvable shape should be
  a loud compile error naming the ascription fix.
- `spec/language.md:298` describes optional `region -> T` as guaranteeing the
  copy-out shape.
- `crates/witchy-lower/src/codegen/mod.rs:4010-4025` says unsupported WIR
  region result shapes fall back to a plain block.
- `crates/witchy-lower/src/codegen/helpers.rs:144-177` documents WIR rcopy
  generation and says recursive cycles return `None`, causing the region arm to
  skip reclaim.
- `crates/witchy-lower/src/codegen/helpers.rs:218-288` implements WIR rcopy
  bodies only for `EqShape::List` and `EqShape::Tuple`; `EqShape::Str` is handled
  separately by `rcopy_str`.
- The same file contains WIR structural helper arms for records, ADTs, and Dicts
  elsewhere, so this is not a shape vocabulary limitation, just an incomplete
  region copy-out implementation.

## Why this matters

`region:` is part of the "Witchy does serious systems/performance work" story.
Silent no-op reclamation for common user data shapes makes the feature feel
fragile and hard to reason about. It also weakens the release narrative because
the docs present `region:` as shipped, while the backend quietly keeps a subset
of result shapes on the fallback path.

This is distinct from recursive `$rdrop`: that issue is about recursively freeing
children when a heap object's refcount reaches zero. This bug is about copying a
region's surviving result out before resetting the region; missing copy-out
reclaims nothing even when a deep drop would not be involved.

## Resolution

The generated WIR copy-out helper now reserves its name before body generation,
so recursive fields compile to calls back into the helper under construction.
ADT helpers dispatch on the runtime tag, allocate the active variant's exact
layout, and recursively copy each payload slot. Dict helpers preserve the
`[hidden index][count][key,value...]` layout, reset the derived index, and
recursively copy every key and value before the region slides down.

Unsupported shape fallback remains only for a type whose concrete `EqShape`
cannot be resolved. Every concrete structural shape represented by the compiler
has a copy-out implementation.

## Expected behavior

Pick one coherent contract:

- implement WIR copy-out for record, ADT, recursive ADT, and Dict shapes; or
- make unsupported explicit/ascribed region result shapes fail with a clear
  diagnostic instead of silently compiling as a plain block; and
- update `rfcs/regions.md`, `spec/language.md`, and performance docs if a
  deliberately partial `region:` shape set is the intended 0.1 contract.

## Acceptance criteria

- `region -> SomeRecord:` and `region -> SomeEnum:` either reclaim via copy-out
  on the compiled backend or fail with a diagnostic naming the unsupported shape.
- A `Dict`-returning region follows the documented contract or is documented and
  rejected as unsupported.
- Region copy-out tests cover at least one record/ADT result and assert
  `__region_copy_bytes` or heap behavior, not only output parity.
- Docs no longer imply full pointer-result copy-out if the implementation keeps a
  shape subset.
