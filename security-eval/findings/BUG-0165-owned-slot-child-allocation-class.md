# BUG-0165: Owned slots accepted unmanaged child storage

`StoreTransfer::MoveUnique` previously required only a unique child token.
It accepted `Bump` child storage into an RC-owned `PointerSlot::Owned`
container, while recursive drop code treats child pointers as RC-managed.

`Scratch` is already rejected at `Alloc(Scratch)` on this path, so it was not
part of the reproduced counterexample. The managed-storage guard also excludes
`Scratch` defensively if a future route introduces such a child.

The backend rejects direct non-RC allocation today, so this was an
accepted-SafeWIR/backend-lowering mismatch rather than an emitted-Wasm exploit.
The checker now requires managed child storage before consuming the source:
concrete RC or the existing immutable RC-or-static provenance union.
