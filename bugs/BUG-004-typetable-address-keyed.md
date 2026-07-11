# BUG-004 — TypeTable keyed by Expr address, consulted after nodes are freed

| field | value |
|---|---|
| Severity | **HIGH mechanism / latent** — potential silent miscompile; no field failure observed, low per-compile probability |
| Status | FIXED ON BRANCH `fix/bug004-typetable-lifetime` |
| Verified | 2026-07-11 CODE: `annotate` now consumes the AST and returns an owning `TypedModule`; the address-keyed table cannot be detached from or outlive that exact module instance. Structural rewrites consume or immediately re-annotate the owner. |
| Component | `crates/witchy-types/src/typeck.rs` + `crates/witchy-types/src/traits.rs` + `crates/witchy-lower/src/codegen` |
| Source | deep-eval fleet Top #15 |

## Symptom (as analyzed)

The TypeTable was keyed by the heap **address** of an `Expr` node. Desugar-temp
subtrees could be recorded and then dropped; later code walked rewritten or
freshly cloned bodies against a detached table and trusted table hits as exact.
Allocator reuse could therefore turn a stale address into a false concrete hit.
On the Wasm path that could silently miscompile instead of rejecting loudly.

## Resolution

The table remains address-keyed, but its required lifetime is now an enforced
ownership property rather than a comment:

- `annotate(Module) -> TypedModule` owns the exact AST and its facts together;
- consumers can borrow the table only through that owner;
- generated monomorphizations consume the old owner and are re-annotated before
  lookup;
- structural `?` rewrites consume the owner, while node-preserving name/op
  rewrites run through a narrowly named API; and
- codegen borrows the table from the same `TypedModule` whose AST it compiles.

The regression test also replaces a typed node and proves the wrapper rebuilds
its facts. There is no longer a detached table that can survive freed nodes and
accidentally accept an allocator-reused address.

Stable AST node IDs remain a possible future representation improvement, but
are no longer required to make the current side table sound: ownership now
prevents address reuse for the entire interval in which facts are observable.
