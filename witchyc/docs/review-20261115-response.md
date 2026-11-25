# Response to the 2026-11-15 quality review

The review (`witchy-quality-review-20261115/report.md`, snapshot `4bfb47f4`)
listed five findings. What happened to each:

| Finding | Disposition |
|---|---|
| Q2 privacy by token spelling | **Fixed** in `4dabf3a7`. Type positions are recorded where the parser reads a type (type parameters in scope and capability rights excluded); constructors and `Type.function` calls are checked where the checker resolves them. A local, field or parameter may share a private type's spelling (`modules/private-name-reused.wy`); a private type named in an annotation or a `Type.function` call from another file is refused. Top-level names stay globally unique, as the review notes is an explicit restriction. |
| Q4 all-pairs recursion analysis | **Fixed** in `4dabf3a7`. Recursive calls are those inside one strongly connected component (iterative Tarjan, linear in calls); unit test with a 200,000-function chain. |
| Q5 HashMap retry loops | **Fixed** in `4dabf3a7`. `settle` and `pour` are private and trap once on a broken internal assumption; `remove`'s unreachable re-placement traps instead of recursing. |
| Q3 sum size grows with all arms | **Fixed after all** (see docs/wasm-sums.md, "Representation"): stored variants share bytes; every write of a slot is conditional on its tags. The first assessment, kept here: **Deferred, with the reason.** Overlapping arms in memory is not local to `sum_layout`: every write of a sum to memory writes every carrier's cell (`write_location`, buffer `init`, frame slots of addressed locals), so overlapping cells would let an inactive arm's carrier overwrite the active one. Doing it needs tag-guarded stores and loads at all of those sites and in frame layouts together, which is a representation change to plan as one piece, not a layout tweak. |
| Q1 proof admission by container-shaped families | **Deferred.** It is the right next architectural step and is not a patch: the storage, memory and record models each recognize their own signature and body shapes. The plan the review gives (element/index value sorts and compositional call effects in the logical IR, one interpreter the models share) stands. No new eligibility family was added since the review. **First step taken** (docs/wasm-proof-memory.md, "Records with several allocations"): the "exactly one Buffer" shape rule is gone from the memory and record models, so a sealed record with several allocations (`pairs/two-buffers.wy`) is proved by the same models; calls that consume, return or mutably borrow such records are modeled too, so `Pairs.push` proves while continuing after `Pairs.grow`. |

Work the review prompted beyond its list: capability checks now fail closed
and see into Buffers (an automated review found that `File<Write>`, once linear,
was storable in a Buffer), and the rule that a capability is not a type argument
is explicit rather than a side effect of storage rules.

Since this response (to `b099d88f`): stored sum variants share bytes (Q3, measured in
docs/wasm-sums.md); records with several allocations are proved, including calls that
consume, return or mutably borrow them (Q1, first steps); and a compiler crash found on
the way (a 2 MiB model thread stack, and two functions passing an unhandled clause back
and forth) is fixed. docs/status-20261116.md summarizes the whole branch.
