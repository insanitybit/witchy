# An introduction to brands

## Which playlist?

You're cleaning up your road-trip playlist and select track **3** to remove it. Before you confirm, you switch to your workout playlist. If the app remembers only the number **3** and applies it to the playlist that's now open, it deletes a different song. Track 3 is a valid position in both lists.

A plain number answers **“which position?”** A branded handle also carries **“which list?”** If you passed a handle from one playlist to a function that expects a handle from another, Witchy would reject the call before the program ran. The number can be in bounds in both playlists.

The graph library uses the same idea. Two graphs can both have a node numbered zero. A brand lets the compiler catch a node from one graph being used to update the other graph's distance table.

## Try it in Witchy

This example uses three song IDs per playlist. Removing a track sets its slot to zero and returns the old song ID, so other positions stay fixed. The selection stores slot `2` because indexing starts at zero.

```witchy
{{#include examples/playlist.wy}}
```

[Download playlist.wy](examples/playlist.wy) and build and run it with the [Wasm commands from the first chapter](start.md). The indexed access needs [Lean proof checking](../docs/wasm-production-build.md). It prints `33`, the third song in the road-trip playlist. Change the last call to `remove(&mut workout, selected)` and rebuild. Witchy rejects the call because `selected` came from the road-trip opening. Both playlists have a third slot.

`remove` takes a playlist and selection with the same `G`. Each `brand` block supplies a fresh `G` and binds the result to the name after `as`.

## Reading your first branded type

Read `Node<G>` as **“a node from group G.”** The letter is a placeholder for an identity the compiler knows. In `Distances<G>`, that same letter means the table uses the same numbering as the node. You don't need to choose a letter at the call site. Witchy works it out from the values you pass.

This helper comes from the [runnable graph fixture](../tests/fixtures/brands/graph-first-step.wy).

```witchy
fn improve<G: Brand>(distance: &mut Distances<G>, node: Node<G>, cost: U64):
    if cost < distance.get(node):
        distance.set(node, cost)
```

If the node and the table come from different openings, there is no single `G` that fits both arguments, so the call is refused.

## Where the stamp comes from

The caller writes `brand` to open the graph's node numbering. Inside that block, the nodes and the distance table it creates share an identity. The graph library declares the opening function with a `Fresh` parameter.

```witchy
brand roads.nodes() as nodes:
    var distance = nodes.distances(allocator, infinity)?
    if let Some(home) = nodes.first():
        distance.set(home, 0)
        for next in nodes.neighbors(home):
            improve(&mut distance, next, 1)
    distance.close()
```

Each opening introduces a distinct type identity, even when you open the same graph twice. `Fresh` introduces that identity. `Brand` lets a helper accept an existing identity. The brand is erased after type checking. A branded value stays inside its opening, even inside a returned record. An ordinary index can leave, and passing it to `nodes.node(i)` in another opening performs a checked lookup there.

## What a brand guarantees

| Question | Mechanism |
|---|---|
| Did this handle come from this opening? | Static brand equality |
| Can I still use this reference? | The borrow checker and the lifetime of the loan |
| Is this index within the storage? | A checked access or evidence for an unchecked access |
| Is this still the same occupant after reuse? | A storage rule such as generation checks or forbidding reuse |

Two arenas can be alive at the same time while having different brands. An index handle can carry a brand without borrowing anything. Resolving the handle borrows the arena, and that loan prevents closing the arena while the resulting reference is still in use.

For a quick exercise, read the [cross-opening refusal](../tests/fixtures/brands/same-graph-twice.refused.wy) and identify the arguments that disagree. Then read the [escape refusal](../tests/fixtures/brands/escape.refused.wy) and identify what carries the brand past the opening.

A collection with a single public operation that always starts at its own root can keep the root internal. Brands become useful when callers keep handles and later pair them with storage or companion state such as a distance table.

[Detailed brand rules and graph API](../docs/design-brands.md) · [Next chapter, arenas](arenas.md)
