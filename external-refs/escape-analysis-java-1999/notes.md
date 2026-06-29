# Escape Analysis for Java

- **Authors / venue:** Jong-Deok Choi, Manish Gupta, Mauricio Serrano, Vugranam C. Sreedhar, Sam Midkiff. OOPSLA 1999. (PDF: Georgia Tech course mirror.)
- **What it is:** A dataflow **escape analysis** built on a **connection graph** abstraction that establishes reachability between objects and references. Decides two things per allocation: can it be **stack-allocated** (does not escape its method) and is it **thread-local** (so locks can be elided). The canonical citation for compiler-driven escape facts.

## Why it matters to witchy

witchy already computes "does this value escape this scope?" in **six disconnected places** (uniqueness share-events, `loop_body_escape_free`, `borrow_escape_check`, the region outer-assignment rule, the lambda-capture scan, region copy-out's watermark check — see `rfcs/performance-modes.md`). This paper is the prior art for consolidating them into **one escape lattice**, and for the payoff of doing so: **escape-driven stack allocation / SROA** (non-escaping records/tuples live in WASM locals, never the heap).

## Informs

- `rfcs/performance-modes.md` — the unified escape/region lattice ("NEXT") and representation tier 2 (escape-driven stack allocation / SROA).
