# Implementation Strategies for Mutable Value Semantics

- **Authors / venue:** Dimitri Racordon, Denys Shabalin, Daniel Zheng, Dave Abrahams, Brennan Saeta. Journal of Object Technology 21(2), 2022 (open access). The Hylo/Val line of work.
- **What it is:** How to compile a language with **mutable value semantics (MVS)** — values that are independent (no shared mutable references) yet mutated in place efficiently — to fast code. Covers part-wise in-place mutation, copy-on-write, and the `let`/`inout`/`sink` parameter-passing conventions that let the compiler avoid copies without exposing references.

## Why it matters to witchy

**This is witchy's design DNA.** witchy *is* an MVS language; its `let`/`var`/`own` conventions are Hylo's `let`/`inout`/`sink` (memory: param-conventions). The paper is the authority for the property that everything else here leans on: *no two bindings share mutable storage*, which is what (a) makes the in-place optimization unobservable (parity) and (b) makes the heap acyclic, the precondition for RC being complete. Cite it whenever justifying why a memory optimization can't change observable behavior.

## Informs

- The whole value model; [`rfcs/ownership-analysis.md`](../../rfcs/ownership-analysis.md), param conventions, the no-shared-aliasing argument for RC.

## Caution

Distinct from the related arXiv preprint **"Native Implementation of Mutable Value Semantics"** (2106.12678) — same group, different paper, LLVM/native focus. This JOT article is the broader implementation-strategies survey.
