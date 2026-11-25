# Brands and scoped identity

A brand ties values to one `brand` opening. The graph library pairs
`Node<G>` with tables and views from the same opening. Two openings of the
same graph have distinct types, even if the graph has the same node count.

[Language guide and example](language.md#brands-and-scoped-type-identity) ·
[Runnable graph example](../tests/fixtures/brands/graph-first-step.wy) ·
[Cross-opening refusal](../tests/fixtures/brands/same-graph-twice.refused.wy)

## Opening and using a brand

- `Fresh` and `Brand` are compiler-recognized bounds. Source cannot declare
  or implement either as a trait.
- `brand expression as name:` supplies a brand to the call at the root of
  the expression, before its arguments are checked. A function can have
  one `Fresh` parameter, including parameters inherited from its impl. Its
  argument types cannot mention that parameter. Calling it outside the
  opening expression or fixing the fresh type through inference is refused.
- `G: Brand` accepts an existing brand, including a caller's abstract
  `Brand` or `Fresh` parameter. Helpers can name `G` in types such as
  `Heap<Visit<G>>`. Callers infer it from their arguments.
- The identity is specific to the opening site at compile time. A generic
  body is checked with abstract parameters, so a recursive helper cannot
  mix its incoming brand with the brand it opens locally.
- Types that carry the brand cannot cross the opening's boundary, including
  through aggregates. An unbranded scalar derived inside can leave. The
  checker erases brand identities before code generation, including nested
  brand arguments, so helpers can share code across openings. There is no
  runtime token or per-execution generation.

[Fresh call refusal](../tests/fixtures/brands/without-brand.refused.wy) ·
[Recursive mixing refusal](../tests/fixtures/brands/recursion.refused.wy) ·
[Escape refusal](../tests/fixtures/brands/escape.refused.wy) ·
[Nested brand erasure](../tests/fixtures/brands/erased-tuple.wy)

## The graph library

`import graph` provides `Graph.new` and `connect` to build a graph with
numeric endpoints. `brand roads.nodes() as nodes:` opens a `Nodes<G>` view
that shares a loan of the graph while in use.

- `nodes.count()` reports the node count. `nodes.first()` and
  `nodes.node(i)` return optional sealed `Node<G>` values.
- `nodes.neighbors(node)` returns a sealed iterator for use in `for`.
- `nodes.distances(allocator, fill)` creates a fallible, owned
  `Distances<G>` table. Its `get` and `set` take a node from the same
  opening. Consume the table with `close` before leaving.
- `nodes.number(node)` projects a node to an ordinary integer. Admitting
  that integer through another view's `node(i)` checks its bounds again.
  Equal counts do not imply equal node order.

Sealing restricts construction to the library. The library checks bounds,
maintains consistent edge vectors and constructs one table entry per node.
Table reads retain their runtime index checks. The shared graph loan
prevents mutation while the view is still used.

[Graph implementation](../library/wasm-graph.wy) ·
[Mutation while open refused](../tests/fixtures/brands/connect-while-open.refused.wy) ·
[Complete Dijkstra example](../tests/fixtures/brands/dijkstra.wy)

## Storage, reuse and ownership

Brand checking is shared by both targets. Running an example also requires
support for its operations on the chosen target. See the
[native coverage guide](native-backend.md#supported-operations-and-trust-boundary)
and [Wasmtime build and launch](wasm-production-build.md).

A brand identifies an issuing scope. Two handles to successive occupants of
one slot could have the same brand. An arena that reuses slots must check
generations or guarantee that old handles are unusable before reuse.
Sealed constructors can restrict issuance to the arena. References are
subject to separate loan rules. A live reference into storage prevents a
conflicting reset or move when that operation needs exclusive access to the
same place. A copied index carries no loan on its own.

Allocation remains fallible. A scope and its brand do not consume owners.
An arena holding nontrivial values must consume them through supported
cleanup before freeing their storage.
[References and loans](wasm-references.md) ·
[Typed storage and release](wasm-typed-storage.md) ·
[Scoped owner cleanup](wasm-using.md)

## Proposed extensions

Unique edit permits and GhostCell-style lending are proposals. They would
need rules that keep both the permit and the cell lent for the resulting
unique reference's lifetime. Per-execution brand identity is also outside
the implemented model. Proving graph membership and table bounds could
justify eliminating a runtime index check in a future library proof.
