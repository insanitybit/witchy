# Design: generators

Status: first version implemented (2026-11-16), docs/wasm-generators.md. The method is
`next_mut(&mut self, cursor: &mut U64)`, as `for`'s cursor form expects. Book: introduction.md (`gen fn squares() -> Iter(Int)`),
appendix-operators.md (`gen` / `yield`).

## What a program writes

```
gen fn squares(limit: U64) -> U64:
    var n: U64 = 1
    while n <= limit:
        yield n * n
        n += 1

fn main(console: Console<Write>):
    for square in squares(5):
        console.print_int(square)
```

A `gen fn` declares what each `yield` hands out (`-> U64`). Calling it runs nothing: it
returns a value holding the arguments, and `for` (or an explicit `next`) runs the body
up to the next `yield`.

## What it becomes

Desugared in the parser's output, before any check, so every later pass sees ordinary
code and nothing new needs to be trusted:

- A sealed record named after the function (`squares` becomes type `Squares`), with
  one field per parameter and per local of the body, and a `state: U64`.
- `squares(limit)` builds that record with `state = 0` and every local at a default.
- A method `next(self: &mut Squares) -> Result<U64, ()>`: the body, flattened into
  `while true: match on state`, where each `yield e` becomes "save the locals, set
  `state` to the point after this `yield`, return `Ok(e)`", and reaching the end returns
  `Err(())` from then on.
- `for` already has a cursor form (src/compiler/iteration.rs, `Way::Next`): it calls
  `container.next(&mut cursor)`, or `next_mut` when the loop is over `&mut`, until `Err`.
  The record's method is `next_mut(self: &mut Squares, cursor: &mut U64)` (the cursor is
  unused; the state is in the record), and `for x in squares(5)` picks `next_mut` when
  that is the only form the type has, binding the call's result to a hidden `var`.

Flattening: the body is cut into blocks at every `yield`, loop head and branch; each
block becomes one arm of the state match, and `while`/`if` become jumps between states.
Only structured control flow exists in the language (`while`, `for`, `if`, `match`,
`break`, `continue`, `return`), so the cut is a walk over the statement tree.

## Ownership and capabilities: the first version's rules

- Values that live across a `yield` are Copy (numbers, `Bool`, Copy records). An owner
  (a `Vec`, a `String`, a `File<Write>`) or a reference held across a `yield` is
  refused: the record would hold an owner the caller could drop mid-sequence, or a
  reference whose loan the checker cannot follow across calls to `next`.
- A `gen fn` takes no capability: the record would carry authority under a name that
  `wasm-caps` would have to see through. `wasm-caps` would still report it correctly (the
  record's fields are read), so this can be relaxed later, with a test.
- A `yield` inside a `match` arm that binds a value is refused in the first version.

These rules make the desugared code ordinary checked Witchy: the checker sees a record
of Copy fields and a method with a loop and a match, nothing it does not already check.

## Later

Owners across `yield` (the record holds them and is linear until finished or closed),
references as part of the record's loan, capabilities, and `take`/`map`-style adaptors
once there is a way to name "anything with `next`" (a trait with an associated type).
