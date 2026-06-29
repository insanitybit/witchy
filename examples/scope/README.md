# scope

Structured concurrency over the cooperative executor. `chan.gather` fans out
result-producing tasks and returns all their results once they have **all**
finished; `chan.scope` is the side-effecting form (run them all, join them all).
Neither lets a handle escape, so a worker can't outlive the scope and there are no
leaked tasks — the safe form to prefer over a bare `spawn` you must remember to
`join`. Deterministic and byte-identical on both backends.

**Shows:** `async`/`await`, `chan.gather` + `chan.scope` (structured nurseries),
first-class channels
