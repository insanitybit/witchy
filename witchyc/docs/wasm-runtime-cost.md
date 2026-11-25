# What a buffer operation costs at run time

Costs here are Wasm instructions counted by Wasmtime's fuel meter, which is
what the host's 1,000,000-instruction budget is measured in. To read them,
build `tools/wasmtime-host` and pass `--fuel`: the host prints the
instructions a run used on stderr. The flag only reports; the budget and
every check are the same with or without it.

## The occupancy walk (fixed 2026-11-14)

The allocator's `validate` helper runs on every buffer operation. It used to
walk the allocation's whole occupancy extent, one byte per 32 bytes of the
allocation, so one access cost time in proportion to the allocation's size
and a loop over a buffer was quadratic.

`validate` now tests the first and the last quantum of the extent, in
constant time. `try_release` makes the full walk once, before it writes
anything.

Why this keeps memory safety. Occupancy bytes live at `[2048, 5120)` and are
written by exactly two helpers: `try_alloc` sets an extent's bytes when it
publishes a live entry, `try_release` clears them when it retires the entry.
Admitted code has no other store into that range: its stores go to its own
stack frame or to a cell address computed from a validated entry. So between
those two calls a live entry's extent stays occupied, and the walk on each
access re-read bytes that nothing could have changed. What the walk did
catch, corruption of the allocator's own state, is still caught at the two
ends on every access and across the whole extent at release.

This narrows a self-check inside the trusted allocator. It is not a proof:
the argument above is the same trusted argument the allocator already rested
on (`docs/wasm-empty-allocator.md`).

Controls: `scripts/test-wasm-allocator.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) zeroes the first, the last and a
middle occupancy byte of a live allocation. The ends trap on any access; the
middle byte traps at release, with no byte written.

## Measured

Instructions used, before (`f62f455d`) and after:

| Program | Size | Before | After |
|---|---|---|---|
| Vec<U64>: push N, pop N, close | 50 | 97,910 | 56,263 |
| | 100 | 277,602 | 111,479 |
| | 200 | 899,402 | 222,104 |
| | 400 | over budget | 443,354 |
| String: push N one-byte scalars | 100 | 220,186 | 201,759 |
| | 250 | 538,376 | 436,718 |
| | 500 | over budget | 874,395 |
| Vec.new(N) then close | 200 | 308,320 | 80,695 |
| | 400 | over budget | 160,345 |

Before, doubling N quadrupled the cost; now it doubles it.
`tests/fixtures/scale/vec.wy` (800 pushes and pops) and
`tests/fixtures/scale/string.wy` (500 scalars) run inside the budget and
could not before.

## Closing a Vec (fixed 2026-11-14)

`Vec.close` emptied every cell of the capacity with a checked take, about
400 instructions per cell whether or not the cell had ever held a value. It
now empties only the first `length` cells, with `memory.take_unchecked`.

Proved: `memory_take_prefix` against its contract, and `Vec.close` releases
its buffer. The proof uses the Vec invariant, that the cells past `length`
are empty, to conclude the whole buffer is empty before release. Four
refusal controls: dropping either precondition of `memory_take_prefix`,
emptying `length + 1` cells, and not emptying at all are each refused.
`vec_buffer_close`, for a bare buffer with no length, still empties every
cell.

| Program | Size | Before | After |
|---|---|---|---|
| Vec.new(N) then close | 400 | 160,345 | 20,139 |
| | 4000 | over budget | 193,839 |
| Vec<U64>: push N, pop N, close | 800 | 885,000 (est.) | 605,648 |
| String: push N one-byte scalars | 500 | 874,395 | 831,210 |

`memory_clear` and `vec_buffer_close` moved to `library/wasm-buffer.wy`, since
Vec no longer uses them. The Vec library has 27 proof goals (was 29). Its
Lean time read 2.0-2.2 s with the machine's load average near 5, against
1.83-1.89 s for 29 goals on a quieter machine, so the time is not shown to
be better or worse.

## Auditing the entry at release, not at every access (2026-11-14)

After the occupancy walk went, `validate` still re-derived every equation
between an entry's fields on each access: requested bytes against capacity
and stride, extent against requested bytes, base inside the heap and aligned,
and so on, about 110 instructions.

Now every access checks what ties a token to its entry: the token is not
zero, its generation is in range, the entry is live, the generation matches,
the type matches, and the count of full cells is no more than the capacity.
That is what catches a stale or mistyped token. The equations are checked by
`try_release`, with the occupancy walk, before it writes anything.

This is the same argument as for the walk, and it is a judgement about the
trusted allocator rather than something proved: an entry is written only by
`try_alloc`, which establishes the equations, and by `try_release`, which
retires it. An access that re-derived them was reading bytes nothing could
have changed. If that premise were false, a corrupted entry could be made
self-consistent, so the equations were not a defence on their own. This
change goes further than the occupancy walk the user asked to have fixed;
reverting it is one function in `src/compiler/allocator.rs`.

`try_alloc` also clears a quantum's four bitmap bytes with one 32-bit store
instead of four byte stores.

Control: `scripts/test-wasm-allocator.mjs` corrupts liveness, generation and
type (every access traps), then occupancy bytes and the entry's base,
requested bytes and extent (release traps, nothing written).

| Program | Size | Start of day | Now |
|---|---|---|---|
| Vec<U64>: push N, pop N, close | 200 | 899,402 | 99,623 |
| | 800 | over budget | 396,623 |
| | 1600 | over budget | 792,623 |
| String: push N one-byte scalars | 250 | 538,376 | 293,291 |
| | 500 | over budget | 585,810 |
| Vec.new(N) then close | 400 | over budget | 12,293 |
| | 4000 | over budget | 116,693 |
| 1000 x Vec.get | | 353,774 | 156,460 |
| 1000 x push and pop on one Vec | | 838,355 | 451,072 |

## What is still slow

Per operation, on the same meter: a bare loop step is 32 instructions, a
`Vec.get` about 150, and a push and pop pair about 440. What remains:

- A call that returns a `Result` builds it, returns it and takes it apart
  again; nothing is inlined.
- Selecting a bit in the occupancy bitmap is a loop of up to seven steps,
  because the admitted instruction set has no shift.
- `try_alloc` scans the occupancy map from the start for a free run.
- A String grows one byte at a time through `Vec.push`; 800 one-byte pushes
  do not fit the budget yet.

## Checked arithmetic in place (2026-11-15)

Every `+`, `-`, `*` and every narrowing to `U8`/`U16`/`U32` used to call one of
six helper functions. Wasmtime does not inline calls, so a loop step was two
calls. The emitter now writes the same tests where the operation is
(`Emitter::checked`, `Emitter::narrow` in `src/compiler/emit.rs`), using three
scratch locals per function. `*` skips the division when both operands are
below 2^32, which cannot wrap.

The instruction meter barely moves, because the tests are the same; time does:

| Program | Before | After |
|---|---|---|
| `while index < 100000000: total += index; index += 1` | 0.44 s, 3.20e9 instructions | 0.05 s, 3.00e9 |
| 50,000,000 steps of `total = (total + index * 3) % 1000003` | not measured | 0.14 s |

Wall time of the engine process on this machine, fuel metering on. The first
row was measured on the same source before and after.

Cost: modules grow (the word-count program from 12,925 to 13,899 bytes). The six
helpers are still emitted, unused, so the function layout the launcher and the
engine check is unchanged.

Still trapping: `tests/fixtures/arithmetic/*.traps.wy` (a sum past the largest
U64, a difference below zero, products that wrap with large and with one small
operand, a U8 sum past 255) and `ok.wy` for the largest results that fit and
zero operands. A `.traps.wy` fixture passes when the program is accepted, prints
nothing and the run reports a trap.

Trusted: the emitter, as before. Proofs reason about the typed body, where
`+` means checked addition; they never read the helper bodies.

## Small functions written into their callers (2026-11-15)

`src/compiler/inline.rs` runs on finished function bodies. A `call` of a source
function that is at most 200 bytes of instructions, calls nothing and does not
touch the stack pointer becomes that function's instructions in a block: its
locals renumbered after the caller's, each `return` a branch out of the block.
A local the callee might read before writing is set to zero first, because a
call would have started it at zero. One result is the block's value; several
leave through a local each, since this profile's blocks have at most one.

| Program | Calls | Written in |
|---|---|---|
| 100,000,000 calls of a five-line function of a record reference and a number | 0.19 s | 0.07 s |
| 1,000,000 pushes, then 20,000,000 `Vec.get` with `items.len()` in the loop condition | 0.17 s | 0.13 s |

What the measurements decided:

- A first version also wrote in callees that call something, such as
  `Vec.get`. It made the second row no faster (0.17 s), the word-count module
  44% larger and the instruction meter 4 to 9% higher on Vec programs. The rule
  "calls nothing" keeps the gain (the 0.13 s is `Vec.len`) at a size cost of
  4% on the same module, and leaves the Vec instruction counts as they were.
- The instruction meter goes up where a function is written in (5.0e9 to
  5.7e9 in the first row) while time goes down: the parameters are stored to
  locals, and the meter counts that. A program that runs close to its budget
  can need a larger `--fuel`.

The allocator's helpers are never candidates, and a source function cannot
name the allocator's memory, so the engine's confinement scan
(`docs/wasm-store-confinement.md`) sees the same six functions naming it. Every
fixture run goes through that scan.

Where the time of a `Vec.get` is: about 160 instructions and 7 ns, most of them
in the allocator's access helper (liveness, generation, type, then the
address). Calls are not the cost. Doing that work once per borrow rather than
once per access is the next step and is a change to the trusted allocator's
interface; it is not started.

Trusted: the pass, with the emitter. Unit tests in `inline.rs` fix the exact
bytes for one result, several results, and the cases left as calls.


## Proved reads without the helper (2026-11-15)

A buffer token is now `((generation << slot_bits | slot) << quantum_bits | first) + 1`,
where `first` is the allocation's first quantum. The entry stores `first` too
(offset 56). Every checked operation still goes through `validated`, which
traps unless the entry's `first` equals the token's; `try_release` audits it
against the entry's base. Allocations never move, so the two cannot drift.

The generation gets the bits slot and first leave, at most 32: with 64 slots
and the default heap that is still 32; with a 4 GiB heap and 65536 slots it
would be 20, a million reuses of a slot before it is retired. The launcher
works out the same `generation_max` and checks it against the receipt.

A read a proof admits (`memory.get_unchecked`, `ref_unchecked`,
`mut_unchecked`) now computes `HEAP_LOW + ((token - 1) & mask) * 32 + index * stride`
inline (`proved_cell` in `src/compiler/emit/storage.rs`). The proof has shown the index is
below the length and the cell full; the token's type is the static type of the
Buffer it came from. What is no longer checked on these reads: the slot's
liveness, generation and type. Those still hold because a Buffer is sealed and
affine (no source code can make, copy or keep a token past a release) and a
borrowed Buffer cannot be released; that is the borrow checker's job, tested,
not proved. If it failed, the read would see stale bytes in memory 0. It could
not touch allocator state, which is in memory 1 (`docs/wasm-store-confinement.md`).
`take_unchecked` and every write that changes a cell's state still call the helper.

Wall time, 2e7 operations, unmetered, best of three, start of process included:

| Loop | Before | Now |
|---|---|---|
| bare loop | 0.047 s | 0.045 s |
| `Vec.get` | 0.134 s | 0.052 s |
| `for item in &vec` | 0.139 s | 0.053 s |
| `Vec.set` | 0.250 s | 0.272 s |
| push and pop | 0.170 s | 0.178 s |

A read is now within noise of the bare loop: `Vec.get` has no call left in it,
so the inliner writes it into the caller. Set, push and pop are unchanged.

Checks: unit tests (235); every fixture (318 runs); `scripts/test-wasm-allocator.mjs`
(23 pass; its token arithmetic now packs `first`, and a token off by one quantum
traps); `scripts/check-i64.py` (2304 exact, 576 trapped, 0 wrong).

## One probe per HashMap lookup (2026-11-15)

`HashMap.get` called `find` and then looked the found cell up again, two
checked reads of the same cell through the allocator helper. It now probes once
and hands back the cell it stops at; its errors are `find`'s. 2e6 lookups in a
map of 1000 `U64` keys: 0.071 s to 0.058 s wall (about 20 ns to 14 ns each,
after process start). A checked read still calls the helper: whether a cell
is full is in memory 1, which emitted code cannot read.
