# Bound generic arguments before building shape memo keys

Status: corrected before submitting the shape-cache integration.
Found by bounded independent review of `53f674b09`, 2026-09-10.

The initial per-query memoization moved cloning and rendering of generic
arguments ahead of their depth-64 and 4,096-node traversal limits. A very wide
tuple argument or deeply nested generic argument could therefore consume
unbounded key-construction resources before the query refused it. Production
reachability of such an oversized argument was not established by the review;
this was not a demonstrated admission of an invalid memory shape.

Argument validation now precedes key construction and memo lookup, as it did
before memoization. Successful body queries remain cached only within one
query, keyed by concrete identity, depth, and the full active recursion set.
No node/depth limit changes, failed-result caching, or backedge-only authority
were introduced. The focused aggregate family includes the actual scheduler
graph, a capability-field near miss, and malformed recursive/generic shapes.
