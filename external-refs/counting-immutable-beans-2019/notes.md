# Counting Immutable Beans: Reference Counting Optimized for Purely Functional Programming

- **Authors / venue:** Sebastian Ullrich, Leonardo de Moura. IFL 2019. (PDF: arXiv:1908.05647.)
- **What it is:** The reference-counting scheme behind **Lean 4** and the direct precursor to the [Perceus notes](../perceus-2021/notes.md). Reclaims memory for non-shared values eagerly and enables **destructive (in-place) update** when rc==1. Its key cost-reduction ideas: **borrowed references** (a parameter that only reads need not touch the count) and a **heuristic to infer borrow annotations** automatically.

## Why it matters to witchy

witchy's `let`-borrow parameter convention is precisely their "borrowed reference" — a read-only param that must not inc/dec (and, in witchy, must not escape). This paper is the argument that **borrow inference keeps RC traffic low**, which is the practical objection to RC-as-floor. Read alongside Perceus: Beans is the simpler, ship-first version of the same identity.

## Informs

- The RC-elision reading of `own`/`let`-borrow conventions ([`rfcs/performance-modes.md`](../../rfcs/performance-modes.md), [`rfcs/ownership-analysis.md`](../../rfcs/ownership-analysis.md)).
- Evidence that a production functional language (Lean 4) runs on precise RC + reuse, not tracing.
