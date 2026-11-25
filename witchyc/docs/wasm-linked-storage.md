# Boxes, weak links, and linked lists

## Owning a box

`Boxed<T>` owns one heap allocation when nonempty.
`memory.try_box(&allocator, value)` returns `Result<Boxed<T>, T>`.
On failure, `Err` contains the original value, including any owners it carries.
Use a result type annotation when the surrounding expression leaves `T` open.

`memory.box_ref(&owner)` and `memory.box_mut(&mut owner)` return a
`Result` containing a shared or unique reference on success, with unit in the
error arm for an empty box. `memory.unbox(move owner)` consumes the box and
returns `Result<T, ()>`. A successful unbox releases the allocation and hands
the payload back to the caller. Handle the payload with its ordinary move and
cleanup rules. A typed `memory.null_box()` creates an empty box with no
allocation. [Box example](../tests/fixtures/thin-links/box.wy)

## Following a weak link

`memory.weaken_box(&owner)` creates a `Weak<T>`. A value may store the link
after the owner has gone out of scope. The link carries no ownership and grants
no access by itself. `memory.follow_weak` takes a shared box loan and
`memory.follow_weak_mut` takes a unique box loan. Both require the compiler to
prove that the link names that loan's live allocation. The resulting reference
keeps the loan active.

The proof uses both address and allocation identity. An allocator can reuse an
address, so comparing only `memory.weak_bits` cannot establish that a saved
link still names the current allocation. [Weak link stored in a record](../tests/fixtures/thin-links/weak.wy) ·
[Same-address control](../tests/fixtures/thin-links/same-address.refused.wy)

## Walking a linked list

`import cell_list` provides a `DoublyLinkedList<T>` that owns its nodes.
`list.push_back(&allocator, item)` returns `None` on success and `Some(item)`
with the original item on allocation failure. `list.close()` consumes the
list and discards its remaining items when `T` supports `Discard`.
`list.close_empty()` consumes an empty list and returns the original owner on
failure.

`list.front()` yields `Option<&Node<T>>`. In its `Some` arm, the
`member_of` postcondition establishes that the node belongs to that list.
`list.next(node)` requires this fact and establishes it for the next node
when present. Call `node.item()` to borrow the payload. Finish using node
loans before removing nodes or closing the list.

Passing a node borrowed from another list to `next` fails the membership
proof, even if both lists have the same element type. The proof derives
membership from the borrowed nodes. An API can also use a
[`brand` opening](design-brands.md) to give related values a shared type
identity. [Traversal and removal example](../examples/node-links/composed-forward.wy) ·
[Foreign node refused](../tests/fixtures/composed-links/foreign-member.refused.wy) ·
[Guarded contracts](wasm-spec-predicates.md)

These examples run on the Wasmtime target. See the
[native target guide](native-backend.md#supported-operations-and-trust-boundary)
for macOS ARM64 coverage.
