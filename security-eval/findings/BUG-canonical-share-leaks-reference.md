# BUG: canonical `Share` mints an untracked runtime reference

## Status

Partially resolved on `impl/rfc0165-canonical-v1`. The operation-level
checker/backend mismatch is fixed; function-exit affine accounting remains
open.

## Defect

`MemoryChecker::share` changes the source object to `Consumed` and creates one
`Shared` destination. The checker therefore models one live token after the
operation.

`emit_share` increments the allocation's reference count and copies the source
pointer into the destination. The source token is never subsequently dropped
because the checker has consumed it. The runtime therefore records two tokens
while the checker tracks one. Dropping the destination leaves the reference
count at one and leaks the allocation and any recursively owned children.

This is a checker/backend correspondence defect, not merely an optimization
issue. `Share` cannot be used as the owned-reference clone required to remove
`CallBorrow` until both sides implement the same token transition.

## Required correction

`CloneRef` now leaves two tracked `Shared` tokens, rejects non-reference-counted
storage, and increments the runtime count exactly once. Focused checker tests
consume both tokens independently, and the executable backend fixture emits
both drops.

The remaining correction is function-exit accounting: every affine token must
be dropped, moved, stored, or returned exactly once. Add a negative checker
test for omitting either token and an executable allocator-reuse probe that
observes reuse only after the second drop. Tail calls must move the caller's
token rather than cloning it.
