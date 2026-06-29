# Perceus: Garbage Free Reference Counting with Reuse

- **Authors / venue:** Alex Reinking, Ningning Xie, Leonardo de Moura, Daan Leijen. PLDI 2021. (PDF: author copy, xnning.github.io; extended TR also at Microsoft Research / UC Berkeley EECS-2021-4.)
- **What it is:** A compiler algorithm that inserts *precise* reference-count instructions (dec at last use, not at scope exit) so that **cycle-free programs are "garbage free"** — only live data is retained. On top of precise RC it adds **reuse analysis**: when a unique (rc==1) object is destructed and a same-shape object constructed, the cell is reused in place. This enables **FBIP** (functional but in-place): write mutating algorithms in a pure functional style.

## Why it matters to witchy

The canonical reference for the **RC-as-floor** direction. Its core precondition — *the object graph is acyclic* — is exactly what witchy's value semantics guarantee (values are snapshots, no back-references, closures capture by value), so RC here is **complete**, no cycle collector. witchy's existing `__cap` token / in-place machinery is a *special case* of Perceus reuse: a statically-proven `rc==1`. Adopting Perceus would make the in-place fast path the floor for *all* heap (bounded memory for long-running servers) while the uniqueness pass demotes to an **RC-elision** consumer (drop inc/dec where uniqueness is proven).

## Informs

- `rfcs/performance-modes.md` — tier-5 "Reuse / FBIP" (PROPOSED), and the open "pick one memory identity" decision.
- `rfcs/ownership-analysis.md` — recasts the cap-token as proven-`rc==1` elision.

## Caution

"Garbage free" holds only for cycle-free programs. The claim transfers to witchy *because* of value semantics; it would not hold for a language with mutable reference cycles.
