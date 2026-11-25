# Memory sizes, fuel and the allocator

## Choosing sizes

A build chooses three sizes.

| Compiler flag | Default | Range | Meaning |
|---|---|---|---|
| `--heap-pages=N` | 3 | 3 to 16384 | pages of program memory; the heap is everything above 98304 |
| `--slots=N` | 64 | powers of two, 64 to 1048576 | allocations alive at once |
| `--stack-bytes=N` | 16384 | multiples of 8, 1024 to 65536 | call frames, below 66560 |

Under Grimoire the three are `[run]` keys, `heap-pages`, `slots` and `stack-bytes`, each
shown as a `limit:` line. They reach the compiler with the grant review: a compiler session's
`prepare-grants INPUT DOCUMENT TEXT [PAGES SLOTS STACK]` checks the sizes and keeps them with
the ticket, and `build-checked` emits the reviewed program with exactly those (2026-11-22).

What runs out, and how it shows: a failed allocation is an ordinary `Err` (a String or Vec
operation returns Err or false), never a trap, so a program that holds many small owners at
once can fail at the slot count well before memory. Each String, Vec, HashMap table, Heap and
Deque alive at once is a slot: a parsed JSON document holds one per string, key, array and
object, so with the default 64 a few dozen members make `json_tree` return 5 (no room); a
968 KB document of 9,000 objects needs about 100,000 (`--slots=262144`). Raise `--heap-pages`
with it (each slot needs memory to point at).

A run is granted what it may use. Defaults match the default build.

| Runtime flag (`witchy-wasmtime-host`) | Default |
|---|---|
| `--fuel=N` (or `--budget=N`) | 1,000,000 instructions |
| `--memory-pages=N` | 4 pages over both memories |
| `--module-bytes=N` | 65,536 |
| `--deadline-ms=N` | 1000, compilation included |
| `--output-bytes=N` | 65,536 bytes of console output (up to 256 MiB), kept until the run ends; up to 1 TiB with `--stream-output` |
| `--stream-output` | off: output is committed when the run finishes. On: passed on a line at a time, and shown whatever ends the run |
| `--fs-entries=N` | 256 files and directories made in a run (up to 1,000,000); each one made or moved also costs 256 of `--fs-bytes` |
| `--dir-id=DEV:INO`, `--data-only` | off; the granted directory must be that very one, and holds no hidden entry and no name a build reads (decisions, 2026-11-22) |
| `--plain-output` | off; with `--stream-output`, control and text-reordering characters are written out and lines beginning `witchy:` or `grimoire:`, as every line of the engine's and the launcher's does, are marked `\| ` |
| `--net-bytes=N`, `--fs-bytes=N` | 1,048,576 bytes each (up to 1 TiB); the first refusal is said once on standard error |
| `--lifeline=fd:N` | the engine ends with status 125 when the launcher's pipe closes, including runs with a deadline |
| `--deadline-ms=none` | no deadline; requires a lifeline |
| `--fuel-per-accept=N` | off; with `--listen`, the count is set to N at each accepted connection and after each whole wait for one |
| `--limit-name=FLAG=TEXT` | off; what a launcher calls one of the limits (`--limit-name=--fuel=[run] fuel`), used in the line said when it is reached |
| `--unmetered` | off |

`--unmetered` runs without the instruction count. The run is then bounded by the
deadline (the engine process exits with status 124), the memory grant and the
1 MiB stack, and not by a number of instructions; it cannot be combined with
`--fuel`. Counting costs about a quarter of the run time: 1,000,000 pushes and
20,000,000 `Vec.get` take 0.15 s counted and 0.11 s uncounted
(`tests/fixtures/scale/unmetered.wy` runs this way; a loop that never ends is
stopped by a 300 ms deadline with status 124). The default is unchanged: every
run is counted unless the caller asks otherwise.

A module larger than its grant is refused before it runs; running out of fuel
is a trap, and nothing is printed. The engine names the trap after the backtrace
(`trap: wasm trap: all fuel consumed by WebAssembly`), and for the instruction
budget says how to raise it; before, a run out of budget showed only a backtrace. `scripts/run-fixtures.py` reads both sets of
flags from an optional `FIXTURE.flags` file (`tests/fixtures/scale/big.flags`).

The allocator's layout follows from the three sizes
(`Heap` in `src/compiler/allocator.rs`). The receipt states slots and stack. (A Node
mirror, `bufferLimits` in `scripts/wasm-host.mjs`, rechecked them for the artifact
tool; both were removed with Node on 2026-11-16.)

| Allocator memory | Bytes |
|---|---|
| entries | 64 per slot |
| hints | 64 |
| occupancy | one per 32-byte quantum of heap |
| cell bitmap | one bit per heap byte |

## What changed in the allocator

All of it is trusted code, and none of it is specific to a container.

- **Shifts and masks.** The profile admits `i64.and`, `or`, `xor`, `shl` and
  `shr_u`. The bit mask of a cell was a loop of up to seven steps; it is one
  shift. Slot counts are powers of two so a token splits by shift and mask.
- **The entry holds the element size and the full-cell count.** An access no
  longer calls `type_info` or reads a separate counts table.
- **Slots come from a list.** A released slot is linked to the one released
  before it (one word in its 64-byte entry, in the allocator's memory), and a
  counter says how many slots have ever been used. Allocation takes the newest
  released slot, or else the next unused one: a fixed number of steps however
  many allocations are alive. Before 2026-11-15 it searched upward from a
  lower bound, which cost a step per live allocation whenever a few slots
  were taken and released in turn, as a String's first growth does. A slot at
  its last generation is not linked and is never used again. Allocation traps
  if the slot it is given is live or at its last generation, so a damaged list
  cannot hand out a slot in use.
- **Quantum hint.** The lowest quantum that may be free. The search for room
  starts there; release lowers it. It is a lower bound, never trusted to be
  exact, so first fit is unchanged: a wrong hint could cost time or report "no
  room", never hand out used memory, because the scan still reads the
  occupancy map for every quantum it takes. This search is still linear in
  the worst case (many small holes below the first hole that is large enough).
- **Eight quanta at a time.** The scan for room, marking, clearing and the
  release audit read or write the occupancy map a word at a time when they can.

## Evidence

- `scripts/test-wasm-allocator.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) (24 controls): 4000 random allocations and
  releases on a 5-page, 128-slot layout agree with a model on the slot chosen
  (newest released, else next unused), the two list words, the first-fit base,
  the occupancy map, and the quantum hint being a lower bound; a slot retired
  at its last generation is never reused, and one forged at it traps;
  unsupported sizes are refused by the compiler; a 16-page module is refused
  without a memory grant, traps on the default fuel, and prints `4999950000`
  with both; other shifts and rotations are still refused.
- `tests/fixtures/scale/big.wy`: 50,000 pushes onto a growing `Vec<U64>` and 300
  Strings alive at once (2 MiB memory, 1024 slots), 58.6 million instructions,
  21 ms in the engine process.
- 3 unit tests on the layout arithmetic.

- `tests/fixtures/scale/churn.wy`: 600 Strings stay alive while 5000 more are
  made, written, read back and closed; both sums are checked.

All 23 controls that need no Lean pass (the 24th is skipped without a Lean
root). Three had gone out of date and were brought up to the byte-output
import and the current borrow message; what they refuse is unchanged.

## Slot list, measured (2026-11-15)

20,000 rounds of an empty `Vec<U8>` reserved to 8 bytes and closed, with other
allocations alive. Instructions, whole program:

| alive | search from a bound | list |
|---|---|---|
| 50 | 57.9 M | 38.6 M |
| 2000 | 887.0 M | 47.8 M |

`tests/fixtures/programs/frequency.wy` on 1 MB of text (135,060 words, 989
distinct): 0.25 to 0.27 s before, 0.10 s after, engine process, unmetered.

## Measured

Instructions, `--fuel` report.

| Program | Before | Now |
|---|---|---|
| Vec<U64>: push 200, pop 200, close | 99,623 | 97,563 |
| Vec<U64>: push 1600, pop 1600, close | 792,623 | 774,587 |
| String: push 500 one-byte scalars | 585,810 | 506,576 |
| `Vec.new(400)` then close | 12,293 | 3,240 |
| `Vec.new(4000)` then close | 116,693 | 23,485 |
| `Vec.get`, each, with its loop | about 156 | about 159 |

"Before" is before store confinement, which added a call to every cell
operation; the allocator changes more than pay for it.

## Still slow

Growing a Vec moves one element at a time through two helper calls, so the
50,000-push program spends about 1,000 instructions a push. A push through the
by-value `Vec.push` also copies the Vec's record in and out.

## Program size

| Limit | Was | Now |
|---|---|---|
| source with everything it imports | 64 KiB | 1 MiB |
| declarations | 128 | 4096 |
| functions in a module (engine, launcher, compiler) | 256 | 4096 |
| function types in a module (engine) | 258 | 4098 |
| module bytes the compiler will write | 64 KiB | 16 MiB |

The JSON fixtures passed 64 KiB of source once the libraries grew, and a
program that imported the JSON library had room for about four functions of
its own under 128 declarations. The limits on one generated proof statement
(64 KiB) and on locals in a function (4096) are unchanged.

The engine still grants a module 64 KiB unless told otherwise, so a larger
module runs with `--module-bytes=N` (at most 16 MiB). The engine's limit on
instructions per module follows that grant, as before.

For an artifact supplied with a separate SHA-384 pin, pass
`--module-pin=sha384:HEX` with 96 lowercase hex digits. The host reads a
bounded regular file without following a symlink at the final name. It
checks the digest of the same in-memory bytes it admits and compiles.
A launcher can use `--module-fd=N` in place of the module path. This
mode requires the pin and an inherited, readable regular file. The host
reads from byte zero, even when the launcher already unlinked the file or
read through the inherited descriptor. The host claims each inherited
descriptor at most once. A module descriptor cannot refer to a secret's
file, even through a separate descriptor.

`tests/fixtures/scale/many-functions.wy`: 300 functions, 74 KiB of source, a
module over 64 KiB; its expected output was computed separately. By hand: 700
functions, a 208 KiB module, 0.09 s to compile with the debug compiler and
0.39 s to run including engine start.

These are bounds on work, not checks of a program's safety. Trusted as before:
the engine's validation walks every function and instruction it admits.
