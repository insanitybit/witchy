# Nullable owned slots: implementation contract

Status: proposed implementation contract; not an admission rule yet.

Recursive sum layouts need an absent child representation. Allocating an empty
child recursively is not a representation of absence. Extending ordinary live
references to include zero would instead weaken every reference operation.

## Boundary

Keep `ValueType::Ref` non-null. Null is permitted only inside a
catalog-declared nullable owned pointer slot. It is not a scalar-to-reference
conversion, a new source-language value, or authority for a projection.

- A nullable owned slot contains either absence or exactly one owned edge to
  its declared pointee layout.
- Initialization of absence is an explicit semantic storage operation, not an
  integer store into pointer storage. It may initialize an empty slot, not
  overwrite a live edge without releasing that edge.
- Storing a reference follows ordinary affine transfer accounting. The source
  token is consumed or cloned explicitly; nullable storage grants no exception.
- A load producing a live reference from nullable storage checks presence at
  runtime before retaining or dereferencing the child. Absence traps. The IR
  operation must state that behavior; neither a sum tag nor a compiler-generated
  instruction pattern implicitly grants non-null authority.
- Destruction skips absent slots and releases each present edge exactly once.
  Non-nullable owned slots retain their existing zero-pointer refusal.
- Range moves transfer absent and present slots with identical source-clearing
  rules. Range clones retain only present references. Byte copying never
  substitutes for either operation.

## Recursive identity

The pointee may be an opaque nominal identity only after its immutable binding
has been checked against canonical source type information. Descriptor hashes
remain acyclic; nominal binding edges may form cycles. Nullable slots do not
authorize unbound, rebound, incorrectly typed, or inline recursive layouts.

## Required executable evidence before admission

1. Empty recursive sum construction terminates without child allocation.
2. A present child survives a checked load and balanced ownership transfer.
3. Loading absence traps before any header access or reference-count write.
4. Scalar stores into pointer slots, live-edge overwrite, and incorrect pointee
   layout are refused independently of lowering.
5. Clone, move, and destruction cover mixed absent, present, and shared edges;
   all three execution legs agree and the ownership accounting closes.
6. Deep recursive values release with bounded call-stack use. The backend
   worklist replaces the older plan's recursive Wasm destructor; stack-depth
   disagreement is not accepted as the intended implementation.

Until these coupled rules exist in checker, oracle, backend, and source
lowering, production nominal admission remains sealed. This contract does not
claim recursive source coverage or compiler-wide memory safety.
