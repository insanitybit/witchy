# Build a tree in an arena

The [packed tree benchmark](../benchmarks/witchy/trees.wy) puts all the nodes of each tree into one vector. Each node stores two 32-bit indices. A zero left index marks a leaf. Slot zero holds the root and is never used as a child.

1. `allocate<G: Fresh>` checks the index width, allocates and initializes the vector, then lends a mutable slice to `build`. Allocation failure is returned as an error. The benchmark driver treats it as fatal.
2. `build` writes the parent followed by the left and right subtrees. In a seven-node tree, the root is at zero, the left subtree is in slots 1–3, and the right subtree is in slots 4–6.
3. `check` borrows the arena and accepts a root with the same brand. `visit` counts through a shared slice, using ordinary checked indexing.
4. `close` consumes the arena and releases its backing vector. The nodes contain only copyable integers, so there is no per-node resource cleanup.

```witchy
fn check<G: Brand>(arena: &Arena<G>, root: Root<G>) -> U64:
    return visit((*arena).nodes.as_slice(), root.index)

fn close<G: Brand>(arena: Arena<G>):
    arena.nodes.close()
```

The driver keeps the long-lived tree open while it builds, counts, and closes temporary trees in separate openings. The code uses fixed slots and never recycles them within an opening. A more general arena must decide how to invalidate old handles after reuse and how to consume owning elements before reclaiming storage. Bulk release of this benchmark's integer nodes is the simple case.

Packing the nodes and freeing one allocation give the performance benefit. The brand is useful if you expose handles across multiple simultaneously open arenas.

[Typed storage](../docs/wasm-typed-storage.md) · [Storage contracts](../docs/wasm-storage-contracts.md)
