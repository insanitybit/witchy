# FP²: Fully in-Place Functional Programming

- **Authors / venue:** Anton Lorenzen, Daan Leijen, Wouter Swierstra. ICFP 2023 (PACMPL vol. 7). (PDF: Microsoft Research preprint `fip.pdf`; also Utrecht webspace.)
- **What it is:** A **linear "fully in-place" (FIP) calculus** that pins down exactly when a pure functional program can run with **zero allocation** — reusing its inputs in place — provided arguments are not shared. Worked through non-trivial data structures: splay trees, finger trees, merge sort, quicksort. The formal backbone for the reuse that [[perceus-2021]] does dynamically.

## Why it matters to witchy

This is the *theory* under witchy's in-place ambition. Where the cap-token decides in-place-vs-copy with a runtime check, FIP gives the **static discipline** that guarantees in-place — which is what `mode opt` wants to *enforce* (and error on violation). The "arguments not shared" precondition is witchy's `unique`/non-escape story; FIP is how you'd make the `unique` surface qualifier (performance-modes tier 3) mean something checkable.

## Informs

- `rfcs/performance-modes.md` — tier 3 (`unique`/`unshared` type) and tier 5 (reuse/FBIP); the "error on de-opt" precision requirement.
