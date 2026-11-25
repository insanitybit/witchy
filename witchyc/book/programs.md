# Contracts, proofs, and packages

## Contracts and proofs

A `requires` clause gives the body a fact the caller must establish. An `ensures` clause gives callers a fact on return. A result match can express different facts on success and failure. For supported forms, Witchy translates these obligations from the checked program and Lean checks them before the compiler publishes an executable.

Read [the source of `Vec.at`](../library/wasm-vec.wy). It accepts any index. Its body tests the bound before calling `memory.ref_unchecked`, and the vector invariant supplies initialization. A caller handles `Ok` or `Err` and can use the corresponding postcondition. Borrowing keeps the vector lent while the returned reference is in use. The native inliner runs after admission, so each caller can use the same contract whether the call is inlined or left intact.

[Source contracts](../docs/wasm-source-contracts.md) · [Scalar proofs](../docs/wasm-scalar-proofs.md) · [What nopanic guarantees](../docs/wasm-traps.md)

## Authority and code organization

An entry point can receive capabilities from the host, then decide what to pass to a library. The arena benchmark's entry point receives console and allocation capabilities. Its recursive traversal receives only a borrowed slice and an index. Follow the parameters to see where authority travels. The Wasm host adds a sandbox boundary, while native executables run with the OS process's authority.

Modules control lookup and visibility. Package policy describes how a program is built and launched. A package's tests can exercise internal logic and use explicitly granted capabilities.

[Capability and launch rules](../docs/wasm-capabilities.md) · [Modules](../docs/wasm-modules.md) · [Packages](../docs/wasm-packages.md) · [Package tests](../docs/language.md#package-tests)
