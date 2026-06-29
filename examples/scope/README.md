# scope

`chan.scope` is structured concurrency: it runs a list of tasks concurrently and
returns only once they have **all** finished. No handle escapes the call, so a
child can't outlive the scope and there are no leaked tasks — the safe form to
prefer over a bare `spawn` you must remember to `join`. Workers report results
over a channel (a task returns `Nil`); the run is deterministic and byte-identical
on both backends.

**Shows:** `async`/`await`, `chan.scope` (structured nursery), first-class channels
