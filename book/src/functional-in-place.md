# Functional-in-Place State Kernels

Ordinary proper tail calls run in bounded stack space. A
**functional-in-place** (FIP) kernel adds a stronger `mode opt` promise: recursive
depth also adds no allocation, reuse, free, or arena-rewind operations.

This is useful for state machines that read and update a small scalar record for
many transitions. You still write an ordinary function—there is no `fip`
keyword or attribute. The ownership signature and recursive shape state the
contract, and the compiler rejects a near miss instead of silently making it
slower.

## A complete kernel

The owner is an `own unique` record whose stored fields are scalar. Every base
path returns that owner, and the recursive call is the function's final action:

```witchy
mode opt

type State:
    count: Int

fn run(own state: unique State, n: Int) -> unique State:
    if n == 0:
        return state
    state.count = state.count + 1
    run(state, n - 1)

fn main(console: Console):
    let done = run(State(0), 50000)
    console.print("${done.count}")
```

```text
50000
```

This example is intentionally deep. Click **Run** in the browser book: the
compiled module completes all 50,000 transitions and the cell displays its
resource counters beneath the ordinary output:

```text
compiled resource counters
rc_alloc_calls 4
bump_alloc_calls 4
rc_reuse_calls 0
rc_free_calls 0
region_rewind_calls 0
```

Change `50000` to `8` and run it again. The result changes to `8`, but all five
counters remain identical. The four allocations are fixed program setup; none
comes from recursive depth. Completing the deep run also demonstrates that the
recursive edge became bounded-stack control flow.

These are exact operation counts exported by the compiled Wasm module—not wall
clock timings. Browser speed, JIT warmup, and machine load therefore cannot turn
the proof green or red.

## The checked shape

The initial FIP contract is deliberately narrow:

- The function is directly self-recursive and has exactly one `own unique`
  owner parameter.
- The owner is a record containing only scalar fields.
- Other parameters are scalar and do not carry heap state.
- The body may inspect and update the owner's fields, but cannot replace or
  escape the owner.
- Every base path returns the owner directly.
- The recursive call is in tail position and forwards that same owner.
- The kernel contains no allocation, effectful helper call, loop, suspension,
  closure, existential construction, or early `?` propagation.

Those restrictions are the proof boundary, not style advice. For example, this
replacement exit is rejected because it discards the incoming ownership token:

```text
fn reset(own state: unique State, n: Int) -> unique State:
    if n == 0:
        return State(0) // error: must return the owned value directly
    reset(state, n - 1)
```

Likewise, binding the recursive result means the call is no longer the final
action:

```text
let next = run(state, n - 1)
next // error: recursive edge is not in tail position
```

The diagnostic names the function and source line and explains which proof rule
failed. Normal-mode code remains free to use those shapes; only `mode opt` turns
the promised resource behavior into a compile-time requirement.

## Verifying outside the browser

`witchy stats` runs the same compiled backend and prints the same exported
counters:

```sh
witchy stats kernel.witchy
```

For a resource regression test, compare a shallow and deep version with
identical setup. Output must remain correct, and this tuple must be equal:

```text
(rc_alloc_calls, bump_alloc_calls, rc_reuse_calls,
 rc_free_calls, region_rewind_calls)
```

The interpreter remains the value-semantics oracle; its Rust representation is
not the resource oracle. Resource claims come from the compiled module's
counters and structural lowering checks.

## Relationship to other ownership features

[Uniform `var` write-back](mutating-methods.md) lets a callee update a caller's
place. `own` transfers a value instead, and `unique` proves that no other live
alias can observe its storage. FIP forwards that ownership token around a
recursive cycle so the record can be updated in place.

[Proper tail calls](tour-functions.md#recursion-and-proper-tail-calls) guarantee
bounded stack for more shapes, including mutual and indirect recursion. They do
not by themselves promise zero heap work. FIP is the narrower resource theorem
for one directly recursive state owner.

The current contract covers scalar-field records. Recursive algebraic data
structures and heap-valued auxiliary state are future extensions; the compiler
rejects them rather than implying a guarantee it cannot prove.
