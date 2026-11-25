# Store confinement

A module with a heap has two Wasm memories.

| Memory | Holds | Who may load and store |
|---|---|---|
| 0, three pages | call frames and the payload bytes of cells | every function except the allocator's helpers |
| 1, one page or more | allocation entries (with each one's count of full cells), two search hints, the occupancy map, the cell bitmap | only the allocator's seven helpers |

The helpers are the last seven functions of the module: `type_info`, `validate`,
`try_alloc`, `capacity`, `try_release`, `cell` and `range` (the range modes, a helper of their
own since 2026-11-18).

## What is checked, and by whom

The engine process (`tools/wasmtime-host/src/main.rs`) checks this on the exact
bytes it is about to run, before compiling them, and without any input from the
compiler:

- a module has one memory of two pages, or two memories of fixed size within the caller's grant;
- every load and store in a helper names memory 1;
- every load and store in any other function names memory 0;
- every instruction is one of a closed list (control, locals, the stack global, twelve loads and
  stores, constants, and unsigned integer arithmetic, comparison, bit and shift operations).
  Anything else is refused, so no instruction that reaches memory another way can pass:
  not `memory.size`, `grow`, `fill`, `copy`, not atomics, not vector loads.

The scan is an allowlist since the second commit; the first version listed what to refuse,
which an automated review rightly called fragile. The launcher's preflight
(`tools/wasmtime-host/src/profile.rs`) applies the same rule to the same list. Either refusal stops the run.

The host itself touches memory 0 only in the bulk operations (`fs_read`, `fs_write`,
`net_send`, `net_recv`): memory 0 is exported as `memory` for them, each range they are
given is checked against its size, and memory 1 is never exported, so the host cannot reach
allocator state either.

A Wasm load or store names its memory in the instruction itself; there is no
computed memory index. So the check is a scan, and what it establishes holds for
every execution: **no instruction outside the seven helpers can read or change
allocator state**, whatever address it computes.

## How the emitter got there

Before this change compiled code loaded an entry's base address and stored
bitmap bytes and counts itself. Now every cell operation calls `cell(token,
type, index, mode)`, which validates the token, tests the index and the cell's
state when the mode asks for it, updates the bitmap and count, and returns an
error code and the cell's address in memory 0. Compiled code then reads or
writes the payload. The proof-admitted operations use modes that skip the index
and state tests; they still validate the token and type.

## Controls

`scripts/test-wasm-allocator.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md), each case put to the launcher and straight to
the engine:

- refused: a program function that stores to, or loads from, memory 1 (four widths);
- refused: a helper-position function that stores to, or loads from, memory 0;
- refused: `memory.size`, `memory.grow`, `memory.fill`, `memory.copy` anywhere;
- refused: a heap module without the second memory, a second memory of another size, a frame-only module naming memory 1;
- accepted: the same modules with each access in its own memory, and a compiled program whose checked init, take, empty take and out-of-range take print `1 7 2 1`.

`scripts/test-wasm-typed-storage.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) drives the `cell` helper directly in every
mode and checks that the first 64 KiB of memory 0, where the metadata used to
live, stays zero.

## The reduced access checks, reassessed

Commits `dc0d63a3` and `c6c5d15f` moved the occupancy walk and the entry audit
from every access to release. The argument was that only allocation and release
write an entry or the occupancy map. Until now that rested on the emitter never
producing a stray store into the metadata region, which shared a memory with
frames and payloads. It now rests on the scan above. The reduced checks stay.

## What is still trusted

- The seven helper bodies. They are identified by position, not by content, so the
  check says nothing about what a helper does with memory 1. A module from
  another producer could put anything there.
- That the helpers return addresses inside the allocation. A wrong address would
  be a store into another cell or a frame in memory 0, not into allocator state.
- Everything about memory 0: that compiled code stays inside the cell it was
  given and inside its own frame is the checker's, the emitter's and the proofs'
  job, as before.
- Wasmtime, its parser (the scan uses `wasmparser`'s decoding), and the host.

## Cost

One call per cell operation where there used to be inline arithmetic. Since
2026-11-15 a read a proof admits is inline again: the token carries the
allocation's first quantum, so the address needs nothing from memory 1 and the
scan's argument is unchanged (`docs/wasm-runtime-cost.md`). With the
allocator rewritten on shifts and the element size kept in the entry
(`docs/wasm-memory-sizes.md`), the cost is below what it was before confinement.

| Program | Before confinement | Confined, first version | Now |
|---|---|---|---|
| Vec<U64>: push 200, pop 200, close | 99,623 | 122,823 | 97,563 |
| Vec<U64>: push 1600, pop 1600, close | 792,623 | 978,223 | 774,587 |
| String: push 500 one-byte scalars | 585,810 | 715,910 | 506,576 |
| `Vec.new(4000)` then close | 116,693 | | 23,485 |
