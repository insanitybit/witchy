# BUG: canonical `Share` mints an untracked runtime reference

## Status

Open. Found while replacing RFC-0165 v1 `CallBorrow` with affine owned
references.

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

Define an explicit affine clone operation. It must leave two tracked shared
tokens, increment the runtime count exactly once, and require each token to be
dropped or moved exactly once. Alternatively, define `Share` to perform that
transition and update every caller and test. Tail calls move the caller's token
rather than cloning it.

Add an executable probe that clones a reference-counted object, drops both
tokens, and observes allocator reuse only after the second drop. Add a negative
checker test for omitting either token from exit accounting.

