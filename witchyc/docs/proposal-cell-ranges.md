# Bulk initialization and moves in `memory`

**Proposed for Fable, 2026-11-18.** Option A adds typed range operations to the existing memory layer. Vec and String implement their library methods on top. The runtime goal is one allocator metadata update per range, with a bounded number of helper calls independent of the element count. Copying the payload still takes time proportional to its size.

## What Witchy already has

- `&[T]` and `&mut [T]` borrow initialized elements. `Vec.as_slice()` and `Vec.as_mut_slice()` cover `0..length`. Spare capacity belongs to the underlying `Buffer<T>`. Forming a slice over empty cells already fails proof checking. [Slice design](design-slices.md) · [Refusal fixture](../tests/fixtures/slices/past-length.refused.wy)
- Vec growth already moves owners. `memory_move_prefix<T: Any>` proves that the destination gets the original values in order, the source range becomes empty, and cells outside the move keep their values. Its body calls `take_unchecked` and `init_unchecked` once per element. This contract is the reference for the bulk move. [Vec implementation](../library/wasm-vec.wy)
- `String.push_text` reserves space once and then initializes one cell per byte. `String.as_bytes()` already provides a `&[U8]` for an owned String. A literal has type `Text` and reads bytes from generated packed constants through `byte_at`. It has no addressable slice in the current representation. [String implementation](../library/wasm-string.wy) · [Literal lowering](../src/parser/text_literal.rs)

## Option A — typed range operations

| Library use | Contract of the memory operation |
| --- | --- |
| Extend a Vec from a shared slice of copyable elements | Copy the slice into an empty destination range. Every destination element equals its source value, in order. The source remains usable. A shared slice of owners cannot authorize duplicating them. |
| Grow a Vec of any storable element, including `String` | Move an initialized source range into a disjoint empty destination range under exclusive access. The destination gets the old values in order, the source cells become empty, and each owner is transferred once. |

Both operations preserve cells outside their ranges. Establish capacity without overflow, including an empty range at the end. Publish the new Vec length after initialization succeeds. Preserve the existing failure contracts, including the original Vec and its owners when reserve fails. The proof translation must connect the actual source values, destination and count to these postconditions. The ownership checker enforces moves and borrowing. Existing range predicates supply vocabulary for these obligations. [Memory proof model](wasm-proof-memory.md)

**Keep the current Wasm memory boundary.** Generated code transfers payloads in memory 0. Allocator helpers alone access the occupancy bitmap and full-cell counts in memory 1. Treat `init_range` and `take_range` as internal bookkeeping steps of a typed operation. Establish all fallible conditions before committing and keep the transfer free of callbacks or suspension while payloads and metadata are being updated. A caller must never be able to mark unwritten cells initialized or discard an owner by clearing its bit. [Host admission](../tools/wasmtime-host/src/profile.rs) · [Store confinement](wasm-store-confinement.md)

Bitmap positions follow element addresses and layout stride. For packed bytes a run can cover whole bitmap words. For wider elements, update only the cell-start bits, preserve neighboring bits and adjust the allocation's full-cell count by the element count. Use the existing admitted payload loads and stores. If the helper set changes, update the emitter and host's positional helper checks together. [Allocator implementation](../src/compiler/allocator.rs)

## First implementation and measurement

1. Exercise copy from a slice and move during `Vec.reserve`. Reuse the [owning growth fixture](../tests/fixtures/vec-string/grow.wy). Check a packed range crossing a bitmap word and a wider element layout. Include an out-of-bounds or occupied destination, an attempted second move and a false contents claim alongside the valid cases.
2. Route `String.push_string` through the slice-backed copy. Its source String already supplies valid UTF-8. For `push_text`, show how the generic operation initializes from the literal's existing byte producer, or leave that path explicitly open. Avoid a temporary heap allocation, a compiler rule for String, or assuming a literal is already a slice. Report each path that actually uses batching.
3. Re-run the existing `str` workload before and after with the same release host, settings and output check. It performs a million nine-byte literal appends, each followed by one byte. Record guest execution and launch-to-exit separately, and confirm from emitted code or a focused counter that allocator calls for an append no longer scale with its length. Measure owner growth separately from String. [Benchmark program](../benchmarks/witchy/str.wy) · [Runner](../benchmarks/run.py)

The recorded M4 baseline (best of five) is **237.5 ms Witchy versus 49.3 ms Go** for the 10 MB String workload, with **222.0 ms** in Witchy guest execution. HashMap took 93.1 ms versus 46.8 ms and binary-trees took 482.8 ms versus 192.4 ms. A contiguous fill or move could also help `Vec.filled` or parts of a HashMap resize. Individually allocated tree nodes have no such contiguous range. The effect of batching has yet to be measured. [Benchmark table](benchmarks.md)

**Where `push_text`'s time goes (measured 2026-11-18, M4, release compiler and host, best of
five guest run times, `--heap-pages=600 --slots=64`, programs in `benchmarks/witchy/parts/`).**
Nine million bytes each way:

| Path | Guest run |
| --- | --- |
| Reading the literal's bytes alone (`byte_at` on packed constants) | 23.8 ms |
| Appending one byte at a time (`String.push`) | 190.4 ms |
| `push_text("abcdefghi")` a million times | 186.8 ms |
| `push_string(&piece)` of a nine-byte String a million times | 158.5 ms |

Reading the literal is about an eighth of `push_text`; initializing one cell per byte is the
rest. A slice-backed copy therefore serves `push_string` directly, but `push_text` gains only
if the literal's bytes can feed a batched initialization of the destination: the per-byte
literal read is cheap, the per-cell initialization is not. No String speedup is claimed until
the same `str` workload is re-run.

**Progress.** Step 1, the move (2026-11-18): the allocator's `cell` helper has range modes 6
and 7, which check every cell of a run before marking any and then adjust the full-cell count
once (helper count and memory boundary unchanged). `memory.move_prefix_unchecked(source,
target, count)` is a proved operation whose model effect is exactly `memory_move_prefix`'s
contract, which is now that one call: one mode-6 and one mode-7 call, then the payload bytes
in program memory, eight at a time. The Vec proof still discharges its 47 goals (1969 ms,
88984 bytes of Lean, against 2168 ms and 94944 before). Three true claims prove and four false
or unsafe ones are refused, as is a buffer moved into itself
(`tests/fixtures/buffer/move-range-*.wy`). Owner growth, best of five guest runs on the same
host, before and after: a million pushes into a `Vec<U64>` 16.4 ms to 14.0 ms, and 200000
Strings into a `Vec<String>` 11.2 ms to 8.9 ms (`benchmarks/witchy/parts/grow-*.wy`). Each
growth makes two helper calls whatever its length; the pushes themselves still make one each,
which is the copy path's part. No String speedup is claimed yet.

Step 3 in part, the copy (2026-11-18): `memory.copy_into_unchecked` is proved and emitted
(one range-mode call; the source's first cell found from its token), with three true claims
proved and four false or unsafe ones refused (`tests/fixtures/buffer/copy-range-*.wy`). Memory contracts
now take two borrowed values holding buffers (one written), so `Vec.extend_copy_mut(&mut Vec,
&Vec)` is proved, and with it `String.push_string` measured 166.7 ms to 117.7 ms for a million
nine-byte appends. It added 370 ms to the Vec library's proof (2483 ms, over the ~2 s budget),
so it is held back as a fixture until the copy's case splits are prelude lemmas. `push_text` stays per byte (186 ms): a literal is not
a buffer, and initializing from its packed bytes in one call is still open. The `str` workload
is mostly `push_text`, so no `str` speedup is claimed.

**Alternative considered.** Option B would allow emitted code to update allocator metadata inline, changing the host-enforced confinement rule. Option A retains that rule and can be evaluated on its own. The original estimate was roughly one day for implementation, focused checks and measurement.

**What a cell operation costs, for option B (measured 2026-11-18, M4, best of five guest runs,
4,000,000 iterations each, programs like `benchmarks/witchy/parts/` with `Vec<U64>` of two).**
A `Vec.new` and `try_release` pair alone: 68 ms, 17 ns. With two `push_mut` and two
`take_last_mut` between: 153 ms, so about 5 ns per push or take. With the helper's bitmap and
count updates skipped (an experiment, not kept): 138 ms, so the bitmap work is about 1 ns of
each 5; the rest is the call chain (`push_mut`, `push_spare_mut`, `memory_init_unchecked`, the
helper) and the helper's entry. Replacing the helper's two `mode % 3` with comparisons moved
nothing (Cranelift already divides by a constant with a multiply). binary-trees runs 481 ms
(Go 192 ms): about 7.5 million allocate-and-release pairs, 30 million pushes and takes, and the
recursion. Option B, metadata updated inline, would save the bitmap call and its ~1 ns per cell
operation, not the allocation itself; the allocation path (first fit from a hint, slot list,
clearing the extent's bitmap) is where a larger gain would have to come from.
Also measured and not kept: removing the allocation and release helpers' three run-time
divisions (`heap / stride` twice and `base % align`, rewritten as a guarded product and a mask)
moved neither the allocate-and-release pair (66.7 vs 68.4 ms) nor binary-trees (477 vs 481 ms).
The divisions are not where the 17 ns goes; finding it needs a profile of the JIT code, not
another guess.

**A profile of binary-trees (2026-11-18).** The host has a development feature,
`cargo build --release --features guest-profile` in `tools/wasmtime-host`, that with
`WITCHY_GUEST_PROFILE=1` samples the guest's call stack every 100 µs from an epoch callback and
prints the functions where samples landed and those on the stack (the default build has none
of it). Epoch checks sit at function entries and loop heads, so the "self" column favours
small, often-called functions; the "total" column is the one to read. For binary-trees:
`build` 62%, `check` 38%; within them `Vec.push_mut` 33% (with `push_spare_mut` 9%,
`memory_init_unchecked` 6%), `Vec.new` 9.5%, `Vec.try_release` 8%, `Vec.take_last_mut` 7%; the
six allocator helpers together about a quarter. No one place dominates: the time is a deep
chain of small calls, each passing a `Tree` as five values, with a helper call at the end. The
levers are fewer call layers on the push and take paths and a cheaper allocation, not the
cell bitmap.

**The range take (2026-11-18, night).** `memory.take_prefix_unchecked(buffer, count)` empties a
run of Copy cells in one mode-7 helper call; the proof asks that `count` fits and the cells
hold values, and afterwards they are empty and the rest unchanged (one proved and five refused
fixtures, `tests/fixtures/buffer/take-range-*.wy`, an owner element among the refusals).
`memory_take_prefix`, and with it `Vec.close` and `String.close` of Copy elements, is that one
call: the Vec proof's Lean module went from 89028 to 86519 bytes, 47 goals as before. Closing
the `str` workload's 9 MB String no longer makes nine million helper calls, but that was a
small part of its time: `str` measured about 4 ms faster on a loaded machine. (The guest
profile counts a sample at each loop iteration's epoch check, so it overweights tight loops
such as the old per-cell take; it showed close at 22%.)

**`push_text` from the literal's own bytes: a design, not built (2026-11-18, night).** A
literal's bytes already come eight at a time from its packed constants (`Text.word(at)`, lowest
byte first), and a `Buffer<U8>`'s cells are contiguous bytes, so one `i64.store` of the word at
the first cell writes them in order. The operation would be
`memory.init_word_unchecked(buffer: &mut Buffer<U8>, at, word, count)`, `count <= 8`: the proof
shows `at + count` fits and the cells are empty; one mode-6 helper call marks them and one store
(or up to seven byte stores for a short tail) fills them. The model gives the run fresh values
(nothing about a String depends on which bytes), which is sound only outside a loop, so the
operation would live in one contracted Vec function, `push_word_mut(&mut Vec<U8>, word,
count)` with `prefix(length) -> prefix(length + count)`, and `push_text` would call it once per
word: a nine-byte append becomes two helper calls instead of nine. No temporary allocation, no
rule for String, no literal treated as a slice. It is not built because the function belongs in
`library/wasm-vec.wy` (Vec is sealed), whose proof is at its budget: the same decision as
`extend_copy_mut` above, prelude lemmas or a larger budget, and both would land together.

**The range fill (2026-11-18, night).** `memory.fill_unchecked(buffer, count, item)` fills a run
of empty cells with one Copy value: one mode-6 helper call, then a store per cell in program
memory. The proof asks that `count` fits and the cells are empty; afterwards they hold the item
and the rest is unchanged (one proved and five refused fixtures, `tests/fixtures/buffer/fill-range-*.wy`,
an owner element among the refusals). `Vec.filled` is that one call (`memory_fill`) instead of a
`put_back` per element. (A first fresh-cache comparison swapped library files without
rebuilding the compiler, which has the library built in, so it measured one library twice; the
measurement below, with rebuilds, covers this change and the next together.) In the HashMap profile below, `Vec.filled<U8>` went from 7.6% of
samples to 0.6%.

**HashMap occupancy tags: tried, not kept (2026-11-18, night).** The map benchmark's profile
puts the allocator's `cell` helper at 21% of samples: each probe asks it whether a cell is
full. A `Vec<U8>` of tags beside the cells (0 empty, else 1..255 from the key's hash, as Go and
SwissTable do) lets a probe read the tag inline and call the helper only on a likely match; the
checked cell operations stay, so a wrong tag could only miss. Measured best of eight, A/B with
the compiler rebuilt for each: 81.8 ms with tags, 77.7 ms without (medians 84.9 and 83.2). The
tags cost a second allocation per table, a fill per resize and a write per insert and take,
and with `U64` keys at this load the probes they save do not pay for that. Reverted; the
measurement stands for the next attempt (a map whose key comparison is costly, `String`, is
where tags would pay first).

**`push_text` a word at a time (2026-11-18, night).** Prompted by a disassembly of the 12:54
`str.wasm` (`/Users/cobrien/workspace/generic/witchy-string-wasm-20261118/str.wat`): appending
`"abcdefghi"` ran `Text.byte_at` and `utf8_spare` nine times, each `utf8_spare` calling the
allocator's `cell` helper, then a one-byte store. Now `memory.init_word_unchecked(buffer, at,
word, count)`, for a `Buffer<U8>` and `count <= 8`, is proved and emitted: the proof shows the
run fits and is empty, the model gives its cells values nothing knows (a fresh function of the
cell, so no proof can lean on which bytes they are), and the code is one mode-6 helper call
then one `i64.store` of the word, or a byte loop for a shorter tail. One proved and six refused
fixtures (`tests/fixtures/buffer/word-range-*.wy`, a `Buffer<U16>` among them). In the Vec
library, `memory_init_word` and `Vec<U8>.push_word_mut(word, count)` (proved, keeps the
invariant); `String.push_text` takes `Text.word(at / 8)` for each eight bytes. The emitted
`push_text` has no `byte_at`, no `utf8_spare` and no per-byte helper call: a nine-byte literal
is two helper calls (the word, then the tail) where it was nine.

The `str` workload, compiler rebuilt for each library, eight guest runs each on one machine
state: 205.6 ms median (203.1 best) before, 157.8 ms (154.1 best) after, the same output. The
rest is the one-byte `push` after each literal and the 10 MB itself.

Proof budget, fresh cache, compiler rebuilt for each: `str.wy` 7.0-8.6 s at `4b8c9a76` (before
the fill) and 7.5-9.0 s now, two runs each twice; `vec`-only 6.6-8.2 s and 7.1-9.7 s. Most of
it is the SDK hashing; the difference is within this machine's spread, perhaps half a second.

**`push_string` a word at a time (2026-11-18, night).** `memory.read_word_unchecked(buffer, at,
count)` reads up to eight byte cells that hold values as one word: the proof shows the run fits
and every cell holds a value, the model gives a number nothing is known about, and the code
finds the first cell from the token as a proved read does (no helper call) and makes one
`i64.load`, or a byte loop for a tail. One proved and four refused fixtures
(`tests/fixtures/buffer/word-read-*.wy`). `Vec<U8>.push_from_mut(source, at, count)` reads a word
of one vector and writes it to another, both operations in its own body, where both vectors'
invariants are in view (a separate `memory_read_word` with `requires` did not have the read
vector's invariant at the call). The range operations' cell checks are now index cases of the
memory model, so the proof introduces the cell and instantiates the invariants at it: at an
offset the invariant is no longer the check word for word. `String.push_string` copies through
`push_from_mut`. A million nine-byte `push_string` calls (`benchmarks/witchy/parts/push-string.wy`),
compiler rebuilt for each, six runs: 145.0 ms median before, 119.3 ms after, the same output;
`push-text` as the control, 118.9 and 119.7 ms.

**Range modes a bitmap word at a time (2026-11-18, night).** After `push_text` went word-wise,
the `str` profile put two-thirds of its samples in the `cell` helper, reached from `Vec.reserve`
(the growth's range move, 41%) and `String.close` (the range take, 15%): the helper still
tested and set one bitmap bit per cell. The bitmap has a bit per heap byte, so for one-byte cells
a run's bits are consecutive. `range_walk` (allocator.rs) now checks and writes an aligned
64-bit bitmap word (64 cells) or byte (8 cells) whole when the stride is one byte and that many
cells remain, and a bit at a time otherwise: the same test (every bit clear for mode 6, set for
7, else a trap) and the same commit, in fewer steps. The host is unchanged (it checks the
helpers' instructions and memory, which these already use). `String/range-alignments.wy` builds,
copies, grows and closes Strings of every length 0..199 beside a neighbour String, and fails for
each of three deliberate mutants of the new tiers (wrong word commit, wrong byte commit, broken
alignment test). That first version cost binary-trees 11% (357.7 to 406.7 ms) and HashMap about as much, though
neither takes the new tiers: the helper is one function, and its larger range code, laid out
before the single-cell path every push and take runs, made that path slower. Choosing the loop by
stride once made it worse (452.9 ms, more code). Laying the single-cell modes out first, in an
`if` that returns, with the range modes after, recovered most of it. Final, compiler rebuilt for
each, six guest runs each, medians against the helper before the tiers: `str` 154.4 to 76.8 ms,
`push-bytes` 178.2 to 76.1 ms, HashMap 79.4 to 79.9 ms, binary-trees 358.1 to 379.3 ms (5.9%
slower, still), the same output everywhere. Short runs cost a few more instructions per bit (the
tier tests), which only an instruction budget sees: the alignment fixture asks for a larger one.

**The range modes as a helper of their own (2026-11-18, night).** The 5.9% left on binary-trees
was the size of the shared `cell` helper. The range modes are now a seventh allocator helper,
`range`, with `cell`'s parameters (modes 6 and 7 only; `cell` takes 0 to 5 again, as small as
before the tiers). The host admits seven helpers (`HELPERS` in main.rs, `ALLOCATOR_HELPERS` in
profile.rs: the last seven functions may name the allocator's memory, and no other may), the
type section has one more signature, and the emitter's range calls go to `range_prepare`.
Against the helper before the tiers, ten guest runs each, medians, load average about 5:
binary-trees 355.5 and 359.3 ms, HashMap 79.2 and 78.4 ms; `str` 158.1 to 77.2 ms and
`push-bytes` 183.2 to 74.2 ms (six runs), the same output everywhere.

**What option B could gain at most (measured 2026-11-18, late night).** The allocator helper's
own share of guest time (`WITCHY_GUEST_PROFILE`, self time of the helper functions), and the
ratio to Go if that time vanished entirely, which bounds what emitting the cell work in callers
could buy (the bitmap work itself stays, about 1 ns of each 5, as measured above):

| Program | Helper self time | Ratio now | Ratio if the helper cost nothing |
| --- | --- | --- | --- |
| words | 36% | 2.4x | about 1.4x |
| trees | 34% | 1.96x | about 1.3x |
| str | 26% | 1.58x | about 1.05x |
| map | 23% | 1.8x | about 1.25x |
| sort | 4% | 1.07x | unchanged |

Option A's remaining step for `str`, one helper call for a whole literal, would save one call of
the three each append makes (about 8% of `str`), and needs a literal's bytes as something a range
operation can read; not taken.
