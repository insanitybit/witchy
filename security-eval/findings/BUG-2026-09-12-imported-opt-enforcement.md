# Imported opt bodies skipped declared performance enforcement

Classification: compiler contract correctness; not a demonstrated memory-safety
violation. Reproduced on 2026-09-12. Fix is candidate-green, not yet landed.

`src/lib.rs::enforce_performance_modes` returned immediately for a normal entry
and otherwise filtered all diagnostics to entry-owned functions. Checking an
opt helper directly rejected its accumulating-snapshot copy cliff. Importing
the unchanged helper from either a normal or opt entry accepted it instead.
The linker already retained `@opt:<module>` origin markers; enforcement did
not consume the existing per-function classification for imported bodies.

Reproducer, `helper.witchy`:

```witchy
mode opt

pub fn accumulate() -> Int:
    var xs = []
    var snapshots = []
    for i in [1, 2, 3]:
        list.push(snapshots, xs)
        list.push(xs, i)
    list.length(xs) + list.length(snapshots[0])
```

Caller, with either no mode declaration or `mode opt`:

```witchy
import helper

fn main() -> Int:
    helper.accumulate()
```

The repair applies the existing declaration-mode query to the four existing
checks: copy cliffs, no-copy misses, FIP restrictions, and parameter ownership
conventions. Normal callers do not inherit imported opt restrictions. No new
qualifier support, memory authority, or canonical no-copy preparation is added.

Evidence: six focused `performance_mode_tests` passed, including normal and opt
importers, a normal-helper control, and a normal caller retaining a snapshot
across an ordinary-var opt call. The rebuilt CLI agrees with the interpreter
on that positive (`1 2`) and rejects all three cliff cases with attribution to
`helper.accumulate`, line 7. Hello, wrap, and jq still agree. Independent static
review found no concrete blocker. Focused log:
`/tmp/witchy-opt-declaration-contracts-20260912.log`.

Limits: this checks existing source performance rules, not actual canonical
allocation placement or optimizer correctness. Qualified-generic origin
attribution remains an untested preexisting concern; this change does not
alter that query. Source `unique` remains outside the agreed initial surface.
