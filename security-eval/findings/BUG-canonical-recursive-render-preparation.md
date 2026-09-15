# Recursive structural rendering exhausts the compiler stack

Status: repaired in the canonical checked-source preparation path; landing pending.

## Reproducer

```witchy
type Tree:
    Leaf(Int)
    Node(Tree, Tree)

fn main(console: Console):
    console.print("${Leaf(1)}")
```

Expected output: `Leaf(1)`. The old lowerer recursively unfolded nominal type
graphs during render-payload and float-import discovery, before executing any
recursive value. Recursive binary/TOML fixtures aborted the Rust test process.

## Repair and boundary

Prepare native compound rendering as ordinary checked helper calls, reserving
concrete helper identities before expanding children. Retain the preparation
session across equality, dictionary, and closed-trait adapter discovery; bind
final callable and proper-tail witnesses afterward. Preserve already-resolved
`Show` calls and structural child rendering, declaration/insertion order,
single operand evaluation, and active-variant-only payload access.

No new encoder, checker, or host authority is introduced. The six tests in
`tests/misc/rfc0165_recursive_render.rs` exercise recursive Int/Float identities,
mutual recursion, dictionary order, custom Show boundaries, operand snapshots,
and late existential adapters. They compare independent expected output against
the source interpreter, SafeWIR interpreter, and admitted Wasm in both modes,
including ownership leak checks. The focused 42-test run also passes the three
previously aborting binary/TOML fixtures; those existing fixtures used the
separately preserved diagnostic admitted-runner migration.
