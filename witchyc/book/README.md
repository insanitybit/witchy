# The Witchy Book

Witchy is a systems language with explicit ownership, capabilities passed as values, and source-level proofs. Start by running a small program, then learn how its authority and memory are managed.

1. [Your first Witchy program](start.md) builds and runs a hello world.
2. [An introduction to capabilities](capabilities.md) reads a real file through a read-only grant.
3. [Records, sums, and matching](types.md) models a reading that may be missing.
4. [Methods and generics](methods.md) builds one API for pairs of numbers or booleans.
5. [Ownership and borrowing](ownership.md) explains cleanup, loans, and methods that borrow individual fields.
6. [An introduction to brands](brands.md) makes the origin of a handle part of its type.
7. [Build a tree in an arena](arenas.md) follows a packed tree from allocation through release.
8. [Prove a small function](proof-tutorial.md) walks through a successful proof, a false contract, and an invalid call.
9. [Contracts, proofs, and packages](programs.md) explains what callers can rely on and links to module and testing guidance.

[Find a language feature](features.md) when you want to look up syntax or a library. Check [native target coverage](../docs/native-backend.md#supported-operations-and-trust-boundary) and the [Wasm host guide](../docs/wasm-production-build.md) before choosing a target.
