# Benchmarks against Go

Benchmarks Game programs written in Witchy (`tests/fixtures/benchmarks/`, `tests/fixtures/f64/`,
fannkuch from the startup harness) and the same algorithms in Go (`benchmarks/go/`), checked to
print identical output, then timed. Launch to exit, best of five, M4, 2026-11-18; Witchy runs in
the release Wasmtime host and includes about 7 ms of engine startup (post-startup execution is
measured separately by the harness in
`/Users/cobrien/workspace/generic/witchy-startup-execution-clock-20261118`).

`python3 benchmarks/run.py --compiler PATH --host PATH` builds each pair (`benchmarks/go/`,
`benchmarks/witchy/`), checks that both print the same bytes, and times them. Witchy's
startup and run are reported apart by the host's `--time` (startup: reading, admission,
engine compilation and instantiation; run: the program itself); the rest of launch to exit is
process launch, writing output and exit. Best of five, M4, 2026-11-18, in ms:

| Program | Size | Go | Witchy | Ratio | Witchy startup | Witchy run |
|---|---|---|---|---|---|---|
| fasta | n = 250000 (2.5 MB) | 41.9 | 51.1 | 1.22 | 3.2 | 36.8 |
| mandelbrot | 2000 x 2000 | 126.2 | 156.9 | 1.24 | 1.2 | 143.6 |
| n-body | 10^6 steps | 38.3 | 55.9 | 1.46 | 5.2 | 41.8 |
| sort | 10^6 U64, generation included | 106.3 | 165.7 | 1.56 | 5.5 | 149.4 |
| spectral-norm | n = 1000 | 40.9 | 78.4 | 1.92 | 7.6 | 59.7 |
| HashMap<U64, U64> | 10^6 counts, 10^6 lookups | 46.8 | 93.1 | 1.99 | 5.1 | 75.3 |
| binary-trees | n = 16 | 192.4 | 482.8 | 2.51 | 5.3 | 464.5 |
| String building | 10 MB in 9-byte appends | 49.3 | 237.5 | 4.81 | 5.0 | 222.0 |

On macOS ARM64, `python3 benchmarks/run.py --compiler PATH --target macos-arm64`
builds the same Witchy sources as standalone executables. It checks exact
stdout and reports paired launch-through-exit medians against Go. Use the
same trusted proof configuration as for the Wasm run. [Native target and runtime](native-backend.md)

Re-run 2026-11-18 (night), same machine and settings, after the inliner, the allocator's type
table and the range operations (Go's own times are up to 10% slower than above, so the machine
was busier; read the ratios):

| Program | Go | Witchy | Ratio | Witchy startup | Witchy run |
|---|---|---|---|---|---|
| fasta | 41.3 | 50.6 | 1.22 | 3.8 | 37.3 |
| mandelbrot | 137.0 | 164.2 | 1.20 | 1.4 | 154.4 |
| n-body | 39.4 | 59.4 | 1.51 | 7.2 | 42.9 |
| sort | 106.0 | 152.2 | 1.44 | 7.1 | 132.7 |
| spectral-norm | 42.1 | 76.4 | 1.81 | 9.3 | 58.3 |
| HashMap<U64, U64> | 55.7 | 99.3 | 1.78 | 6.2 | 80.7 |
| binary-trees | 204.0 | 378.9 | 1.86 | 6.4 | 358.4 |
| String building | 56.1 | 221.7 | 3.95 | 5.9 | 204.8 |

binary-trees went from 2.51 to 1.86 times Go, and sort from 1.56 to 1.44. String building is
still the outlier, and it is `push_text`'s per-byte cell initialization (see
proposal-cell-ranges.md), whose fix waits on the Vec proof budget.

spectral-norm with its indexes stated below 2^20 (`requires` on `a` and the loops, now in
`benchmarks/witchy/spectral.wy`): the entry's `s * (s + 1)` is proved in range, so its
overflow checks are not emitted. The prover bounds a product through the names' definitions
(`s = i + j`, `t = s + 1`; `scalar/product-of-sums*.wy`). Guest run, best of six, A/B on one
machine state: 57.8 ms before, 35.6 ms after, the same output.

Array indexes in proved functions (2026-11-18, night): a function with a proof also proves each
array index below its length when the facts already bound it (`i` of `for i in 0..5`, `j` under
`while j < 5`), and when every index of the function is so proved its bounds checks are not
emitted (`scalar/index-proved.wy`; an index the facts do not bound keeps its check,
`scalar/index-unproved.traps.wy`). n-body's `advance` with `ensures true` and `proof: auto`:
guest run 43.5 ms to 39.7 ms (best of six, A/B), its 29 checks gone, the same output.

Re-run at the end of 2026-11-18 (after proved index checks, the spectral-norm and n-body
bounds, the range fill, and `push_text`/`push_string` a word at a time), same settings:

| Program | Go | Witchy | Ratio | Witchy startup | Witchy run |
|---|---|---|---|---|---|
| fasta | 40.6 | 49.6 | 1.22 | 3.9 | 37.0 |
| mandelbrot | 137.3 | 162.3 | 1.18 | 1.4 | 152.8 |
| n-body | 38.8 | 54.7 | 1.41 | 7.0 | 39.8 |
| spectral-norm | 42.3 | 51.6 | 1.22 | 9.0 | 34.3 |
| sort | 106.4 | 149.2 | 1.40 | 7.1 | 132.9 |
| HashMap<U64, U64> | 52.4 | 99.6 | 1.90 | 6.2 | 78.0 |
| binary-trees | 200.7 | 377.4 | 1.88 | 6.2 | 357.3 |
| String building | 50.4 | 172.3 | 3.42 | 6.4 | 156.9 |

Against the morning's table: spectral-norm 1.92 to 1.22, n-body 1.46 to 1.41, String building
4.81 to 3.42, binary-trees 2.51 to 1.88. The largest gaps left are String building (one helper
call per appended byte for the single-byte `push`, and the 10 MB itself), HashMap (a helper call
per probe; tags measured no faster) and binary-trees (allocation).

binary-trees profiled again at the end of 2026-11-18: `Vec.push_mut` 25% of samples, `check`
22%, `build` 20%, the allocator's `try_release` 15%, `try_alloc` 10%, `cell` 9%. `try_release`
audits the whole entry before it writes anything (every field equation, its place in the heap,
the occupancy of its extent: docs/wasm-store-confinement.md). Measured by removing it (not
kept): 369.8 ms with the audit, 348.1 ms without, median of eight, about 6%. The audit is a
deliberate check against a corrupted entry and stays.

Where the gap is:

- spectral-norm: about 23 ms is the overflow-checked `U64` arithmetic of the matrix index
  (Go's `int` does not check; computing it in F64 gives 58 ms). The `Vec` read itself is cheap:
  in a plain summing loop `v[j]` costs about 0.36 ns over no read at all (20 million reads:
  26.1 ms against 18.9 ms), a bounds check and a load as in Go, and `get` without the trap or
  `at` are no faster, so the `Result` that `index` routes through is folded away by the engine.
  Replacing the read with a constant in spectral-norm (50 ms) removes more than the read, as it
  also changes what the engine can schedule; hoisting the loads out of the loop did not help
  (decisions, 2026-11-18). A contract
  removes proved checks (decisions, 2026-11-17), but here it would need the nonlinear `s * (s +
  1)` proved and a contract on every caller, so that is not the answer for ordinary code.
- HashMap: about 10 ms of Witchy's time is launch, startup (5 ms) and exit, and lookups are about 12 ms; the rest
  is inserting, where each new entry and each entry moved by a resize changes a cell's state
  through the allocator helper. A table of `Empty | Full` slots in a `Vec` (probes as proved
  reads, inserts as trades) was prototyped: 85 against 89 ms, since filling each new table
  with `Empty` pushes through the same helper. Not adopted; the lever is the same as
  binary-trees'.
- String: every byte appended marks one cell full through the allocator helper. This, HashMap's
  inserts and binary-trees' nodes are one cost; docs/proposal-cell-ranges.md proposes the fix.
- binary-trees: every allocation and release goes through the allocator helper (the occupancy
  bitmap lives in the second memory, which emitted code does not touch).

**2026-11-18, later: fewer call layers.** A guest profile of binary-trees (host feature
`guest-profile`, docs/proposal-cell-ranges.md) showed its time spread over a deep chain of
small calls rather than in the allocator's bitmap. The inliner now copies in a small function
that calls only imports or helpers, and takes callees up to 400 bytes over four rounds. Best of
five guest runs, same host, every output identical:

| Program | Before | After |
| --- | --- | --- |
| binary-trees | 477 ms | 367 ms (Go 192 ms) |
| `str` | 225 ms | 206 ms (Go 49 ms) |
| sort | 148 ms | 133 ms |
| map, spectral-norm, fasta, mandelbrot, n-body | unchanged | |

Modules are 25-45% larger and start about 1 ms later. Measured and not kept: the allocator
helpers' run-time divisions and `mode % 3` (no change).
Then, the same night: the allocator's type table is written into allocation and release
instead of called (binary-trees about 4% faster again, about 375 ms on interleaved runs), and
`Vec.close` of Copy elements empties its cells with one range call (`str` a few ms; the machine
was loaded, so no exact figure).

## map, measured 2026-11-18 (late)

At 1.7-1.9x Go (run 75 ms). Profile: the per-cell helper (`cell`, modes 0-2, audited) is 17% self,
`get_mut` 11%, growth (`pour`, `place`, `settle`) about 24% in total. Measured:

- Wrapping the probe index with a compare instead of `% capacity` per step: no change (75.6 against
  74.9 ms, release builds per variant). Not kept.
- Growth: the same program with the table sized up front runs in 53 ms against 76. Each moved entry
  costs about four audited helper calls (take, the probes, init).

What would cut it is fewer audited calls per probe: skipping the token audit for checked reads, as proved
writes already do, or a storage operation that inits the first empty cell from an index. Both change
what the allocator helper trusts or does, so they are left for a decision rather than taken here.

### Validating a buffer once per shared borrow: measured, not taken (2026-11-18)

The question was whether a buffer reached through a shared borrow, which the borrow checker keeps
live and unchanged for the borrow, needs its token audited on every checked read. Two numbers decide it.

- Ceiling: a throwaway compiler whose checked read (`cell` mode 2) skips the audit entirely, as
  proved operations do. Release builds, 5 interleaved runs, medians: growing map 76.8 to 68.5 ms
  (-11%), presized 49.1 to 45.1 ms (-8%). Repeatable, but this is every audit removed.
- What one validation per borrow would keep of it: each probe loop is its own call (`find`,
  `get`) under its own borrow. An exact simulation of the benchmark (same hash, linear probing,
  3/4 load, `2c+8` growth) gives 1.50 checked reads per call while growing and 1.17 presized. One
  validation per call therefore removes 28% and 15% of the audits: about 2.3 ms (3%) and
  0.6 ms (1%), before the per-probe "validated yet" test it would add.

That is not a whole-program win worth a new mechanism in trusted emission, so it is not taken.
A larger share would need the validation hoisted across calls (a callee specialised as
"pre-validated"), which no benchmark here justifies. Presizing (`HashMap.new(allocator, n)`)
remains the demonstrated lever when the caller knows the size: 76 to 53 ms. Simulation:
`benchmarks` session scratch `audit/sim.py`, reproduced in the text above.

## sort: the library sorts a slice (2026-11-18, late)

Profile: `sort_range` 54% self, `sort_swap` 20%; the allocator helper 3%. Each comparison read the
Vec's length and token through `items` twice, at every step of the partition scans, though
nothing in a scan writes to the Vec; the Wasm compiler cannot know that and does not hoist the loads.
A slice keeps its start and length in locals. The same introsort written by hand over `&mut [U64]`
ran in 81 ms against the library's 134 (nine interleaved runs, medians), the ceiling.

The library's sort now works on `items.as_mut_slice()`: comparisons through the slice's `at`,
exchanges through `cell` and `memory.exchange` with the held element, so any element type,
owners included, sorts as before. Release builds per variant, benchmark harness: run 133.1-134.5
to 101.5-102.1 ms; the whole program from 1.40x Go to 1.10x. The rest of the gap to the hand-written
version is the generic comparison through `at` and the three-way exchange that owners need.

## Startup is Wasmtime compiling the module; it now compiles in parallel (2026-11-18, late)

`WITCHY_TIME_PHASES=1` with `--time` now splits startup: nbody's 7.8 ms was read and admission
0.3 ms, Cranelift compilation 7.5 ms, linking and instantiation 0.03 ms. The host built Wasmtime
without its `parallel-compilation` feature, so functions compiled one after another. With it
(nothing about what runs changes), nbody's compilation is 3.5 ms against 7.4 (medians of five), and
the suite's startup falls by 2-6 ms each: spectral 1.23x to 1.11x Go, nbody 1.46x to 1.33x,
sort 1.11x to 1.06x, fasta 1.30x to 1.24x; run times unchanged.

What is left is the largest function's compile time. In nbody that is the library's f64 printing
(`Console.write_f64`, 1806 lines of text format, with `write_big`, `shifted_down`, `shifted_up`),
larger than the program; a smaller one would cut startup for every program that prints a float.
Caching compiled code across runs would remove compilation altogether, but it is a trust decision
(precompiled modules), parked for the user.

### str: `String.byte_or_end` through a slice, measured, not kept (2026-11-18, late)

The checksum loop reads each byte through `byte_at`, which makes a `Result` and takes it apart.
Reading through `as_bytes()` instead (as `Span.byte_or_end` does) changed nothing: run 79.4-79.7
against 79.9-80.8 ms, release builds per variant. The code generator already removes the
`Result`. Profile: the range helper (word-wise appends) 32%, the checksum loop in `main` 44%,
whose per-byte `% 1000000007` Go's version computes too.

### str: the range modes' token audit, measured, kept (2026-11-18, late)

Proved single-cell writes and proved reads decode their entry from the token without the audit;
the range modes (6/7, used by `fill`, `push_text`, `push_string`) were written to audit always.
A throwaway compiler whose range modes decode as the proved single-cell ones do: `str` run 75.2-77.0
to 72.6-72.9 ms (-4.5%, release builds, three alternating pairs). Kept as written: a few percent on
programs dominated by range writes does not pay for removing a defence the range code chose on purpose.

### str: range marks a bitmap word at a time, at any offset (2026-11-18, late)

Profile of `str`: `String.push_text` 50% of samples, most of it in the range helper. Each append
of a word (`memory_init_word`, eight bytes or fewer) checks and then marks its cells in the
occupancy bitmap; the helper took a whole bitmap word or byte only where the run started aligned
to 64 or 8 cells, else a bit at a time, and `str` appends 10 bytes a line, so most appends were
up to 16 single-bit steps (check, then commit). One-byte cells now take, each step, as many bits
as remain in the bitmap word holding the next one, under a mask: the same test, one load or store
per word. Same output; A/B of the same program's Wasm, three alternating pairs: run 75.7-87.7 to
64.6-68.1 ms. All benchmarks, best of 5: str 76.9 ms (Go 48.7, 1.58x); the rest unchanged within
noise. Test: `tests/fixtures/string/range-marks.wy` (appends of 1..17 bytes at every offset beside
a live neighbour, then close and reuse); a build marking one bit too many, and one marking one too
few, each trap on it.

### Checked `x * k` by one comparison with `MAX / k`: measured, not kept (2026-11-18, late)

A constant multiplier lets the overflow test be `x > MAX / k` instead of the product divided back
(taken only when an operand is 2^32 or more). `str`'s `check * 31` already takes the fast path:
62.2-66.9 against 62.2-63.7 ms. A loop of 20M products with a 2^32-sized operand: 41.4-42.6 against
42.5-43.4 ms, within noise (this machine divides fast). Not kept; the edge fixtures
(`arithmetic/mul-constant*`) stay.

### `for` over a Vec or slice without a test per element (2026-11-18, late)

`for x in &v` (1M `Vec<U64>`, 100 rounds, summing): 88.6-106.4 to 57.5-57.8 ms (1M pushes alone:
about 7 ms), so 0.99 to 0.51 ns per element. Through `v.as_slice()` before the change: no faster
than `&v`, the cost being the `Result` per element and the checked counter, not the Vec's token
decode. The suite: unchanged (no benchmark is dominated by walking a Vec).

### `String.chars`: an ASCII fast path, measured, not kept (2026-11-18, late)

`for c in text.chars()` over 1.2M mixed ASCII and Latin text, ten rounds: about 6 ns a character
(75.9-76.0 ms). A trivial counting generator costs 2.25 ns a step, so the machinery is about a
third of it and `scalar_at` (a `Result` of a record, not inlined) most of the rest. A fast path
in `chars` that yields an ASCII byte directly, skipping `scalar_at`: 92.4-96.7 ms, slower. It adds a
`yield` (a state) and a local the record saves and restores each step, which costs more than the
call it skips. Not kept. The lever is the generator itself: `next_mut` is a call per step that
loads and stores every local of the record; a `for` that steps a generator it made could keep
those locals in its own frame instead (a design change, not taken here).

### A `for` over a generator runs its body in place (2026-11-18, late)

The lever the `String.chars` measurement pointed to. Counting generator, 12M items: 27.0-27.3 to
4.1 ms. `for c in text.chars()`, 12M characters: 70.1-71.7 to 47.4-51.6 ms (the rest is
`scalar_at`, now the next thing to look at). Outputs identical to stepping.
`String.scalar_at` returns an ASCII byte at once (the same `Scalar(lead, 1)` the full decode gives):
`chars` over the same text, now in place, 49.5-52.9 to 44.3-46.8 ms.

### Word frequencies; `utf8_spare` no longer calls itself (2026-11-18, night)

A text workload (1.2M generated words, `words()`, `HashMap<String, U64>` looked up by `Span`):
73 ms; `String.push` 48% (each byte marks its cell in the allocator's helper, by design), the map's
lookup by Span 32%. `utf8_spare` called itself on the failure `utf8_room` rules out, so it was never
inlined; it now stops with `unreachable` there (the same trap the endless recursion ended in, and
now one the trap report lists): 72.6-74.2 to 69.5-70.9 ms.
`span_is` comparing slice against slice instead of `key.byte_at` per byte: 71.0-71.2 against
71.1-71.6 ms (three-byte words). Not kept. What is left in the lookup is the probe itself
(`find_by`, a call per probe) and hashing.
Added to the suite as `words` (Go: `strings.Fields` and a `map[string]uint64`): Go 34.1 ms, Witchy
84.1 (2.47x; run 72.1).

### What is left in `words` (2026-11-18, night)

Self time: the cell helper 35% (one call per byte pushed, marking the cell full and counting it),
`Span.equivalent` 18%, `main` 15%, the probe 9%. The helper's proved-write path is about 25
instructions; what costs is the call per byte. Emitting that path in the caller would write the
allocator's metadata memory from program code, which today only the generated helpers do: a
trust-boundary decision, parked for the user, not taken here.

`for i in 0..s.len(): s[i]` (checked) against `for x in s` (unchecked walk), 100M elements: 57.2 to
57.4 against 56.7 to 59.0 ms. A bounds test costs nothing measurable; the walk's gain was the
per-element `Result`. Nothing to do for index loops.

### Interpolation costs nothing over direct writes (2026-11-18, night)

`console.print("n=${i} sq=${i * 3}")` against `write`/`write_u64` pieces, 300k lines, same bytes:
31.3-31.9 against 31.9-32.7 ms. The format record is inlined away.

### Instruction budget per String operation (2026-11-18, night)

Fuel counts every Wasm instruction. `String.push` of mixed 1-4 byte scalars: about 793 a push;
`chars()` (stepped, here a while-built String): about 392 a character, so 2.5 KB of text walked
after being built needs over a million, the default budget (a review's program ran out). The
emitted code's local traffic is 38% of instructions statically (`local.get`/`set`), constants
13%; of that, removable without changing anything: dead stores 5% of stores, set-then-get pairs
2% of instructions, plain copies 7%. About 12% at most, for a pass over the Wasm, and nothing at
run time (Cranelift removes them). Not taken; the default budget (parked) is the lever.
Tried: dropping a `const 0; local.set x` that is `x`'s first mention outside any loop (also
matching a run of constants to the run of sets a multi-slot value is stored with). No fuel saved:
`String.push` 324 a byte either way. The zeroing on the hot path is of value slots that whole-value
copies read (a variant's inactive slots), so it runs on every call, inlined or not. Reverted.

### Compile time by program size (2026-11-18, night)

Generated programs of 100 and 400 small functions (1,000 and 4,000 lines): 30 ms each to compile,
startup 9 and 15 ms; the 400-function module (72 KB) needs the host's `--module-bytes` above its
64 KB default. One function is limited to 4,096 operations (now said with what to do).

### Suite at the end of 2026-11-18 (night), with a regression check

Best of 5 (the machine was loaded: Go itself 5-7% slower than earlier the same day): fasta 1.20x,
mandel 1.21, nbody 1.30, spectral 1.05, sort 1.07, map 1.80, trees 1.96, str 1.54, words 2.60.
The afternoon's compiler against this one, same machine, alternating: trees 385.8-388.3 against
388.1-389.8 ms, map 79.8-82.8 against 80.6-82.3 ms: no regression from the day's changes.

### `match` dispatch: a chain, measured, no jump table (2026-11-18, night)

A ten-arm `match` over a sum, 3M dispatches through a slice of variants: 3.8 ms (1.3 ns each). The
arms are an `if` chain; the branch predictor carries it, and a `br_table` would mispredict as
an irregular chain does. Not taken.

### spectral-norm faster than Go (2026-11-18, night)

The kernels took `&Vec<F64>` and looped `for j in 0..n`, so each `v[j]` kept its bounds check
(`Vec.index`'s `Err` arm). Taking `&[F64]` and looping `for j in 0..v.len()` makes each read a
proved slice read: the path says `j < v.len()`, so the read is a goal and its check goes, and
`v.len() < 2^20`, tested once at entry, gives the matrix entry's `requires`. Same output
(1.274224148). Run 34.5 -> 25.0 ms; best of 5 against Go: 43.5 vs 38.9 ms, ratio 0.89 (was 1.05).
The writes `out[i] = sum` keep their check (writes are not proved).

### Compile time of the benchmark programs (2026-11-18, night)

Warm proof cache, release compiler (MBX, 34 s build): about 15 ms a program by phase (`wasm-measure`):
parse 3.5 ms (90 KB with the library), checking 7-10 (typing 4-6, proof goals 2-3, planning 1-2),
proof cache 2-3, emit under 1. The debug compiler the fixtures use takes 40-56 ms end to end. No
phase stands out; nothing changed.

### Closures cost nothing measurable (2026-11-18, late night)

`fold(&v, start, fn(a, b): a ^ (b + 1))` with `F: WitchyFn2<U64, U64, U64>` against the same loop
written in place, 10M elements: 5.0 ms each (best of three). A generic function is specialised
for each closure type and the call is inlined, so the closure is not an indirect call.

### Compile time after the night's checker additions (2026-11-18, late night)

Release compilers, the generated 4,000-line program (400 functions, each with a loop): checking
12.8-13.8 ms with the afternoon's compiler against 13.4-15.9 ms now (counted and guarded reads,
`&Vec` loops, while heads, collected refusals); parse 6-7 ms either way; 30-40 ms end to end.
Within noise; nothing to take back.

- **HashMap probe step without `%` (2026-11-18, not taken):** `index = (index + 1) % capacity` replaced by an increment and a compare at all five probe loops, A/B with rebuilt compilers, best of 8: `map` 75.1 -> 75.0 ms, `words` 69.9 -> 70.2, Astra's 75% churn 19.4 -> 19.4. Probe runs are short at these loads, so the per-step division is not where time goes; reverted.

- **Cold compile time after the 2026-11-19 library proofs** (`hash_bytes`, slice `String.eq`, `write_string`): release compilers at ff29c886 and now, empty proof cache, two runs each: a String-key lookup program 8.28/7.32 s -> 7.86/8.08 s, `words` 7.51/7.58 -> 7.70/8.32 s; within run-to-run noise. Warm (cached) compiles 0.02-0.03 s.

### Suite, 2026-11-19 (after hash_bytes, slice String.eq), best of 3
fasta 1.32x Go, mandel 1.19, nbody 1.18, spectral 0.79, sort 1.09, map 1.76, trees 1.92, str 1.43, words 2.20. Profiles of the three furthest: `words` is String.push building its input (52%, the per-cell helper: Option B, parked); `trees` is allocation and the cell helpers (#38/#40/#41 ~34%, push_mut 25%); `map` grows from 16 cells (pour+settle 22%) and probes twice on each of its 432,577 misses (`get_mut` fails, then `put` searches again). A probe-once insert-or-update (`entry().or_insert()`) needs an API decision, since an owner key must be handed back on a hit, or a vacant-slot token must stay valid; not added, for the author.

- **`byte_or_end` over the slice, not taken (2026-11-19):** the `str` benchmark's checksum calls it per byte: 60.2 -> 59.6 ms, under 1%. A lone `byte_at` is cheap; tonight's text gains came from not decoding and re-encoding scalars and from leaving the by-value (proved) push paths, not from the Result itself.

### Suite, 2026-11-19 late (after the text-library pass), best of 3
fasta 1.19x Go (1.32 earlier the same night: `Console.write` reads literals a word at a time), mandel 1.16, nbody 1.33, spectral 0.94, sort 1.09, map 1.54, trees 1.94, str 1.51, words 2.08. Within run-to-run noise elsewhere; the three slowest are bound as recorded above (cell helper, allocation, `map`'s second probe).

- **Where a small program's time goes, and serial compile (2026-11-19, not taken):** a 420-byte module runs in 8.8 ms wall, of which process spawn is ~5.5 (as for `/bin/echo` from the same harness), read+admit 0.09, engine creation + compile 1.0-1.3, link+instantiate 0.03, run 0.02 (`WITCHY_TIME_PHASES=1`). Turning off parallel compilation: -0.1 ms on that module, +1.0 ms startup on `fasta`; reverted. Caching compiled code would remove compile, but that is the parked "trusted precompiled modules" decision.

### A failed allocation scans the heap (2026-11-19, for the author: the allocator is the reviewed TCB)
Growing a `Vec<U64>` by `push_mut` in a 200-page heap: 524,284 pushes succeed in 5.9 ms; once a doubling no longer fits, each further push retries the growth and fails, at ~55 µs a failure (1 failure 5.9 ms, 10: 6.3, 100: 11.2) — consistent with a scan of the occupancy map (one byte per 32-byte quantum, ~409,600 here) per request. 76,000 failed pushes take over 4 s, so a loop that keeps pushing after memory is exhausted runs into the deadline instead of failing quickly. A free-quantum count kept beside the search hints would let a request larger than all free memory fail at once; not made, as it changes the reviewed allocator. With room, by-value `push` is 13.5 ms against `push_mut`'s 11.1 ms for a million pushes (not worth reworking).

### Tonight's compiler against 4862ebf7 (2026-11-19, best of 5, release builds of both)
Module bytes / startup ms / run ms, before -> after: fasta 7745 -> 7745 / 2.3 -> 2.0 / 35.4 -> 35.3; mandel unchanged (1143 B, 138 ms); nbody 15268 -> 15268 / 38.8 -> 38.9; spectral 18921 -> 19141 / 24.9 -> 24.8; sort 18255 -> 18665 / 96.7 -> 96.6; map 12860 -> 13040 / 71.1 -> 71.9; trees 12011 -> 12171 / 356.3 -> 357.3; str 15257 -> 15597 / 61.4 -> 61.5; words 26041 -> 26521 / 70.2 -> 69.7. Modules 0-2.2% larger, the marks on traps written into callers (9d2b0885); startup and run time within run-to-run noise. The proof-report and message changes cost nothing on a build that proves. Compile time with a warm proof cache, best of 5: words 49 -> 48 ms, spectral 45 -> 45, sort 49 -> 48.

### Baseline for the derive goal: a four-field key, derived Hash against hand-written (2026-11-19)
`HashMap<Key, U64>` with `Key` four U64 fields, sized once for 200,000 entries; 200,000 inserts then 1,000,000 lookups (800,000 hits). `derive(Eq, Hash)` (each field `(x % p) * k`, folded by rotate-xor: four remainders) 49.7 ms; hand-written `Hash` (fields mixed by shift and xor, one `hash()`: one remainder) 48.8 ms; best of 5, same results. The derived hash is within 2% of one written by hand, so the "four-field key as fast as handwritten" criterion is about the generated Eq/Hash/law proof, not the hash's speed; what is left of the time is the probe and the field-by-field `eq`. The third example (a borrowed view finding an owned String without allocating) already runs through hand-written `impl Equivalent<String> for Span` / `for Text` (library/wasm-span.wy, wasm-string.wy; `hashmap/span-lookup*.wy`): what the derive adds there is generating that impl and proving its hash agrees, not the lookup itself. The second example (an id-only key whose other fields may differ), hand-written today (`hashmap/id-only-key.wy`): 200,000 inserts and 1,000,000 lookups with `User{id, visits, level}` keys whose Eq/Hash read `id` take 17.3 ms against 15.8 for plain U64 keys (best of 5, same results); the hash and compare are the same work, so the 10% is the larger entry (24 bytes of key against 8) moved and probed (a guess: the profile shows the time in `main`, with the lookups written in, and in the cell helper; it does not separate them). The same profile shows the map's teardown: `HashMap.discard` of 266,000 cells of U64 keys and values is 8.8% of that run (~1.4 ms, ~5 ns a cell), one cell-helper `take` per cell though nothing in them needs discarding. `Vec.close` empties its cells in one range step (`take_prefix_unchecked`), which needs every cell of the range full; a map's are scattered. A range mode that empties every cell whatever its state would make a copyable map's teardown one call; it is a new mode of the allocator's cell helper, the reviewed TCB, so it is left for its author (with the fail-fast count recorded above).
- **`HashMap.get` with a contract, measured, not taken (2026-11-19):** `ensures true`, `proof: auto` and `invariant index < capacity` prove, and remove the probe's checks (`index + 1`, `% capacity`, `step += 1`): fuel -0.9% (U64 keys 768.4M -> 761.2M), wall time unchanged (U64 keys 16.80 -> 16.78 ms, id-only keys 17.85 -> 17.81, `map` 70.1 -> 71.0; best of 7/5, a release build per variant). As for fasta's generator, the checks are well-predicted branches; a contract pays where it lets the code do less work (`hash_bytes`'s one reduction per six bytes), not where it only drops a test.

### Suite again, after the console and text changes (2026-11-19, late; best of 5, same run)
Against 4862ebf7 in one run (the machine about 15% slower than the earlier run: mandel 138 -> 163 ms for both): mandel 163.7 / 163.1, nbody 42.8 / 42.4, spectral 27.4 / 27.4, sort 103.5 / 102.6, map 79.9 / 79.4, trees 384.2 / 387.3, str 68.8 / 69.0, words 77.8 / 75.9 ms; fasta (now through a WordWriter, which 4862ebf7's library lacks) 35.4. Modules 2.6-12% larger. In words (+3.1 KB of code), by function: `String.eq` 522 -> 1538 B (the word path with two `word_at` reads written in, beside the short-string loop), `Console.write_u64` 96 -> 901 (the digit packing), `Console.write` 729 -> 1271, `String.word_at` and `Vec.word_at` 468 new; each once per module, nothing copied into callers, and startup unchanged. The night's gains are in programs that write text, format numbers or look up long keys, which the suite does little of.
- **One byte through the word path is slower than through `push`, measured, not taken** (2026-11-19): `show_scalar` (JSON's punctuation) sending ASCII through `push_ascii(scalar, 1)` made JSON writing 287 -> 331 ms (+15%; best of 3, same output). A word write is one range-mode helper call, dearer than the single-cell one `push` makes; it pays from a few bytes on (`read_line`'s runs, words of literals and digits), not for one. Fusing JSON's punctuation (`":`, `,"`) into two-byte writes is therefore unlikely to gain; not tried.

- **End of 2026-11-19, the whole suite against Go** (`benchmarks/run.py --runs 3`, every output byte-equal to Go's; release compiler at cb5c47c1+): fasta 0.97x, mandel 1.19, nbody 1.35, spectral 0.86, sort 1.01, map 1.58, trees 1.95, str 1.38, words 1.89 (2.08 earlier the same day). Against the start of the day's compiler (4862ebf7), program by program and interleaved: no benchmark slower beyond noise. The four slowest (trees, map, words, str) are bound as recorded: allocation and the per-cell helper (Option B, a single-cell owner type), and `map`'s second probe (an insert-or-update API). A note for measuring by hand: build with the runner's per-program sizes; `trees` built with `--slots=4096` ran in 2 ms with smaller trees, as a failed allocation is an Err the program carries on from, and only the runner's comparison with Go's output catches that.
- **Late 2026-11-19, the suite again** (`run.py --runs 3`, outputs byte-equal; release at 0ece459d), Witchy run time (host `--time`) and ratio launch-to-exit: fasta 33.0 ms 1.08x, mandel 143.7 ms 1.23, nbody 39.2 ms 1.20, spectral 25.0 ms 0.85, sort 98.6 ms 1.04, map 70.8 ms 1.47, trees 353.0 ms 1.88, str 63.2 ms 1.53, words 70.2 ms 2.50. `words` and `str` looked slower than this morning (1.89, 1.38) and are not: built by each compiler saved during the day (before the inliner change, before `String.push`'s, now), their run times are equal (words 70-72 ms, str 61-62 ms), and repeated runs give words 2.10-2.50x and str 1.37-1.53x as Go's own time moves (words 33.9-40.0 ms). Compare Witchy's run time across days, not the ratio; the ratio is Go's noise as much as Witchy's.
- **Night of 2026-11-19, after the code-size passes** (constant-multiplier checks, fewer zeroed locals, no zero offsets, final returns falling off, `local.tee`, dead local writes; release at 61f9fe17; outputs byte-equal to Go's), Witchy run time and ratio: fasta 32.4 ms 1.05x, mandel 143.0 1.22, nbody 39.0 1.16, spectral 25.0 0.88, sort 98.4 1.05, map 70.5 1.54, trees 356.4 1.89, str 62.7 1.43, words 70.0 2.16. Run times as in the late-evening run (trees +1%, within its spread): the passes made modules 4-5% smaller and cost nothing at run time.
- **After the scoped zeroing** (release at 3876b849; outputs byte-equal to Go's): fasta 32.3 ms 1.06x, mandel 148.3 1.16, nbody 39.2 1.26, spectral 25.1 0.83, sort 99.6 1.09, map 70.4 1.73, trees 355.0 1.92, str 63.3 1.38, words 69.0 2.07. Run times as before the pass (modules 2-6% smaller). mandel's 148.3 was a slow sample: its module differs from the earlier build only by two empty `else`s removed, and ten interleaved runs each give 141.7 vs 142.2 ms means (minimums 140.9, 141.1).
- **End of the night (55bc8fd8), not measurable**: run with the machine's load average at 5-10 from other applications, Go's own times rose 10-20% (fasta 40.5 -> 48.4 ms, sort 108.6 -> 117.7) and interleaved runs of `map` from the same module spread 101-146 ms. No figures recorded from it; the last valid run is the one after the scoped zeroing above. To repeat on a quiet machine: the compact-locals and room-rule changes were measured neutral on words, nbody and sort, and modules are 7-12% smaller.
- **End of the night, repeated once the load fell** (55bc8fd8 code; one-minute load 2.7 after a 26-minute wait; outputs byte-equal to Go's): fasta 31.5 ms 1.02x, mandel 140.0 1.23, nbody 39.5 1.29, spectral 25.1 0.87, sort 95.9 1.07, map 76.0 1.73, trees 357.8 1.93, str 62.7 1.54, words 70.1 2.56. Against the run after the scoped zeroing: within noise, sort and mandel lower. map's 76.0 (70.4 there) is the machine, not the code: eight interleaved runs of map built before the last three changes and after give minimums 75.2 and 75.1 ms. The ratios move with Go's own time (words 32.2 ms here, 41.1 there).
- **Afternoon (54bfd5e4; bulk copy restored, hash policies, derive recipes, key law; load 2.6 after a 7-minute wait; outputs byte-equal to Go's)**: fasta 32.4 ms 1.00x, mandel 143.1 1.18, nbody 39.3 1.24, spectral 25.1 0.83, sort 98.4 1.05, map 72.8 1.68, trees 354.8 1.85, str 62.1 1.50, words 70.0 2.33. Against the end of the night: every run time within noise (map 76.0 -> 72.8, sort 95.9 -> 98.4). The restored bulk copy does not reach these programs: `str` appends literals (`push_text`), and `words` neither appends Strings nor extends Vecs; its gains are in `push_string` and `extend_copy_mut` users (3.4x and 1.25x in Astra's measurements).
- **Evening (67b670b1; one-probe `get_mut`, ASCII-first `String.push`, span helpers through the map, division-free probing; load 2.6 after a 3-minute wait; outputs byte-equal to Go's)**, Witchy run time and ratio: fasta 30.4 ms 1.12x, mandel 134.9 1.16, nbody 38.6 1.27, spectral 24.5 0.89, sort 92.8 0.99, map 65.5 1.51, trees 354.0 1.91, str 59.1 1.43, words 60.8 1.86. Against the afternoon: words 70.0 -> 60.8 ms, map 72.8 -> 65.5, str 62.1 -> 59.1, sort 98.4 -> 92.8 (sort and str unchanged in code; the machine), the rest within noise. Still bound as recorded: trees (allocation, cell helpers: Option B), map's second probe on a miss (insert-or-update API), str and words (the per-cell helper).
- **words after the evening's changes, profiled** (2026-11-19, 60.8 ms): 48% self in the allocator's cell helper (the one function every cell read and write goes through: `String.push`, `HashMap.get_mut`/`find`/`settle`/`place`/`pour`, `Vec.push_spare_mut`), `get_mut` 16%, `String.push` 10%, `Span.hash` 6%. The hot calls already pass the proof-admitted mode that skips handle validation; what remains per call is the cell's address (stride and base from the entry), the occupancy bit's load and store and the live count: the metadata writes Option B would inline (parked for the user). Removing the remaining `+`/`-` overflow checks was measured to gain nothing (decisions.md). The library levers outside Option B are spent for this program.
- **Odd HashMap growth on the suite's maps** (2026-11-19, 4be67ea0; eight interleaved runs, load 7): map 85.9 -> 86.5 ms mean (minimum 79.2 -> 76.2), words 63.5 -> 63.6; the same output. Unchanged on the suite's dense keys; the gain is on strided U64 keys (the map benchmark with keys `* 1024`: 164.9 -> 101.6 ms; decisions.md).
- **sort on adversarial orders** (2026-11-19, 1,000,000 U64s, the sort benchmark with its fill changed, one run each): random 103.8 ms, sorted 32.9, reversed 32.3, all equal 43.4, organ pipe 150.2, two values 47.2. No order is quadratic; organ pipe, median-of-three's weak case, is held to 1.45x random by the heap-sort fallback.
- **Lookup-only checked reference and `get_or_insert`** (2026-11-19, 3804c55b and f727299f; release builds at the benchmark flags, interleaved runs, load 5-7, the same output): the cell helper's mode 2 without the token audit: map 96.4 -> 86.5 ms median (paired ratio 0.917), words 0.97-0.98, trees 0.97, str 0.99, the rest within noise. `get_or_insert` (one search for insert-or-update) in map and words: 0.977 and 0.996.
- **Grouping a four-U32 key's hash input** (2026-11-19, a scratch program, not a suite benchmark): 3M hashes of a Quad, four 4-byte feeds 59.6 ms, two 8-byte words 31.7 ms (median of 11), the same hashes. Not available to derived map keys: the key law accepts one feed per field (decisions.md).
- **Packed U32 words in a KeyMap key** (2026-11-19, a scratch program at --heap-pages=512, release compiler, seven interleaved runs): a four-U32 key in `KeyMap` under Fast(7), 200,000 puts then 2.5M gets: fed one field at a time 136.8 ms median, fed as two packed words 115.2 ms (ratio 0.84), the same output. Now accepted by the key law (decisions.md). Seen on the way: at the default heap (3 pages) the table cannot grow past 1,528 cells, `put` then fills the old table and every miss probes all of it (2.5M gets: 20 s); that is the heap size, not the map (`--heap-pages`).

## Compile time: four hot spots from a profile (2026-11-20)

Release compiler, `wasm-measure` medians of 21 interleaved runs. The machine's load average was
13 to 19, so take the absolute numbers loosely; the comparison is interleaved.

| program | parse before → after | check before → after | total before → after |
|---|---|---|---|
| `http/request.wy` | 8.67 → 7.23 ms | 17.09 → 15.26 ms | 29.9 → 26.3 ms (−12%) |
| `programs/paths.wy` | 4.75 → 4.19 ms | 11.26 → 10.37 ms | 21.7 → 20.0 ms (−8%) |
| `benchmarks/k-nucleotide.wy` | 6.85 → 5.77 ms | 14.98 → 14.11 ms | 27.2 → 24.6 ms (−9%) |

What was slow, from `samply` (`devtools/samply_top.py` aggregates its profile):
- **Symbols.** The lexer found a symbol by trying 37 strings in turn. It now matches on bytes.
- **Constants.** `constant::lift` found a token's file by counting module starts, once per token, and built a key per `a.b`. It now binary-searches and looks up by reference.
- **Effects.** The effect check scanned every function for each trait implementation, twice. It now indexes required methods by trait, and formats names only for a refusal.
- **Others.** `Checker::others` scanned every signature to find `name@file` keys. It is now an index built once per program. Plain calls no longer copy a signature's defaults.
- **Hex.** A digest's hex took a `format!` per byte. It now uses a table.

All 1,740 fixture programs compile to byte-identical modules, with identical refusal messages,
before and after (`devtools/prof/same.py`).

## Compile time: allocation, and repeated work over the library (2026-11-20, evening)

Release compiler, `wasm-checked`, 21 interleaved rounds (order alternating), medians of CPU time
(user + system, `getrusage`) and of instructions retired (`/usr/bin/time -l`). Instructions are
the steadier measure on this machine: CPU time moved 20% with load (5 to 9) between batches,
instructions about 1%. So a change under about 1% of instructions is not counted as one.

| program | CPU before → after | instructions before → after |
|---|---|---|
| `scale/many-functions.wy` | 15.81 → 15.55 ms (−2.6%) | 200.5 → 195.2 M (−2.4%) |
| `f64/print-exact.wy` | 29.48 → 28.80 ms (−2.8%) | 310.7 → 294.0 M (−5.4%) |
| `pairs/two-buffers.wy` | 24.04 → 23.23 ms (−4.8%) | 227.4 → 213.8 M (−6.0%) |
| Coven `digest.wy` | 22.64 → 22.44 ms (−1.2%) | 220.0 → 205.4 M (−6.6%) |
| Coven `fetch.wy` | 27.51 → 26.25 ms (−2.3%) | 279.6 → 263.8 M (−5.8%) |
| Coven `publish.wy` | 27.45 → 26.17 ms (−3.9%) | 282.4 → 265.8 M (−5.8%) |
| Coven `sign.wy` | 25.59 → 24.81 ms (−3.2%) | 254.7 → 239.6 M (−6.0%) |
| Coven `verify.wy` | 26.05 → 24.50 ms (−4.3%) | 250.5 → 235.4 M (−6.0%) |

What the profile shows (samply at 4 kHz over the Coven programs and `print-exact.wy`, with a
line-table build and `dsymutil` run from the target directory so `atos -i` names source lines):
- **The library is most of every build.** A two-line program costs 25 M instructions; `hello.wy`
  (which imports the console, and so Vec and String) costs 200 M; the seven-line Coven
  `verify.wy` 235 M. Every build parses, checks and states the goals of the same library
  functions again. Only a checked-library cache would change that, which is a design question
  (what is trusted, what is keyed), not a tuning one.
- **The system allocator is 30% of busy CPU**, spread over every phase (a quarter of it in
  `realloc` from growing strings and vectors). Measured, not adopted: the compiler linked with
  mimalloc 0.1 is 12–16% less CPU and 25–27% fewer instructions on the same programs. The
  compiler has no dependencies, and adding a C allocator is a supply-chain decision, so it is
  left to that decision.
- **Freeing at the end** (the syntax tree, the logical program consumed by planning) is about
  8% of busy CPU in drop glue.
- **Opening files** is expensive here: `open` alone is 2.6% (the proof cache's two files, the
  bundle's sources, publishing).
- **Threads** wait little: the main thread's join is about 6% of samples while the record,
  memory and scalar models run.

Kept (all 1,803 fixture programs and the five Coven programs give byte-identical modules,
diagnostics and `wasm-checked` reports, including every goal's statement and the proof-cache
hit count, against the build before; `devtools/prof/same.py` agrees):
- **Record goals read each text once.** `Model::finish` tested every ghost name against the
  whole goal text, then each validity fact against every ghost, again for every path. Ghost and
  term names are single identifiers (`gN`, `vN`), so it now reads each text's identifiers once
  and looks them up; the ghosts each validity fact names are found once; paths are grouped
  first and read once per group, not once per check. The 2026-11-19 note (decisions.md) found
  no gain from an identifier set in wall time, since this runs on worker threads; in CPU time
  and instructions it is about 4% (the line-level profile put `mentions` and the set building
  at 6% of busy CPU before).
- **The parser moves past a token without copying it.** `Parser::take` cloned the token (and
  its text) on every call, though most callers drop it: 1.7% of instructions.
- **Constants: only constant names are collected** as names a file declares itself (the only
  names that set is asked about).

Tried and not kept:
- Hashing a proof-cache key in parts instead of formatting the module once more, and checking
  the prelude's namespace once per process instead of per goal: −0.2% and −1.0% of
  instructions on two programs, inside the noise. Reverted.

## Compile time: what the library costs each build, by phase (2026-11-21)

Release compiler at 7f24f293, warm proof cache (no Lean started), `scripts/library-cost.py`.
Instructions retired (`/usr/bin/time -l`) are medians of 11 to 15 runs; load average was 4 to
24 during the runs, so the phase times (medians of 11 `wasm-measure` runs, at load 4) are
loose and the instruction counts are the measure. No change was made to the compiler, so
there is no after (decisions.md, same date: why no checked-library cache is landed).

| program | instructions | of which the program's own |
|---|---|---|
| two lines, no import | 26.4 M | |
| `import console`, empty `main` | 188.7 M | 0 |
| the same, one `console.line` | 190.7 M | 2 M |
| `console/hello.wy` | 204.2 M | 15.5 M (8%) |
| `import console, crypto, fs`, empty `main` | 207.8 M | 0 |
| Coven `digest.wy` | 210.3 M | |
| Coven `sign.wy` | 244.9 M | 37 M |
| Coven `verify.wy` | 242.9 M | 35 M (14%) |
| Coven `fetch.wy` | 267.2 M | 59 M |
| Coven `publish.wy` | 270.8 M | 63 M |

"The program's own" is the difference from the same imports with an empty `main`
(`digest.wy` imports other modules, so it has no such row). The import alone is 162 M over
the process's 26 M: what is imported is paid for, not what is called.

Phases, in ms (wall, one thread except goals):

| program | read | parse | typed check | goals | concrete | proof lookup | emit | report | publish |
|---|---|---|---|---|---|---|---|---|---|
| two lines | 0.21 | 0.09 | 0.06 | 0.05 | 0.04 | 0.00 | 0.02 | 0.01 | 0.38 |
| console, empty `main` | 0.41 | 2.39 | 3.20 | 2.71 | 0.98 | 1.37 | 0.02 | 0.10 | 0.65 |
| `hello.wy` | 0.78 | 2.28 | 2.98 | 2.48 | 0.95 | 1.28 | 0.33 | 0.09 | 1.11 |
| Coven `verify.wy` | 0.41 | 3.12 | 4.56 | 2.70 | 1.60 | 1.53 | 0.48 | 0.12 | 0.87 |

Busy CPU of the empty-`main` console program (samply at 40 kHz, 40 builds, 18,689 samples,
all threads): parsing 20.1%; typed checking 34.5%, 30.3% of it in function bodies; the goal
threads 15.4%; the proof phase 9.0%, of which finding each goal in the cache (building its
Lean text and hashing it) is 7.2%; concrete planning 7.5%; the rest is reading, writing the
247 KB report, publishing and the process. The program has 131 goals and 192 selected
functions and emits 439 bytes.

Whether a program changes the library's goals (`scripts/library-prefix-stability.py`): all
1,677 fixtures with an expectation built in 12.7 s at eight jobs (31 s user, 16 s system; 849
accepted, 828 refusals). Among the accepted, 80 distinct lists of imported standard modules
and 7,585 pairs of list and library method: every pair has the same goal statements in every
program. The seven reported differences are programs' functions with a library free
function's name.

## TOML: `import toml` against toml-spanner, toml and toml_edit (2026-11-25)

One parse of each of toml-spanner's own benchmark inputs and a generated 1 MB file
(`benchmarks/toml/data/`), native code on an M4, release builds (toml-spanner 1.0.3, toml 1,
toml_edit 0.25, rustc 1.100). `benchmarks/toml/run.py` runs each parser and its baseline once
a round, one after the other, for nine rounds, and reports the median of
(run − baseline) / parses; Witchy's baseline copies the text without parsing it.

| input | bytes | witchy | spanner | toml | toml_edit | witchy / spanner |
|---|---|---|---|---|---|---|
| zed | 18311 | 53.3 µs | 15.7 µs | 113.6 µs | 156.8 µs | 3.40 |
| zed-lock | 277542 | 636.6 µs | 188.0 µs | 1385.5 µs | 2049.1 µs | 3.39 |
| extask | 4027 | 16.8 µs | 4.9 µs | 38.9 µs | 55.1 µs | 3.45 |
| devsm | 2220 | 9.2 µs | 2.7 µs | 17.0 µs | 26.5 µs | 3.41 |
| big | 1000248 | 3945.8 µs | 1639.8 µs | 9271.3 µs | 12845.2 µs | 2.41 |

**The target, toml-spanner's time, is missed: Witchy takes 3.4 times as long on the four
real files and 2.4 times on the 1 MB file.** It is about twice as fast as `toml` and three
times as fast as `toml_edit`. The first working version took 190 µs on `zed` (13 times
toml-spanner); what brought it to 53 µs is listed below.

Where the time goes (`sample`, `zed`, native build with `WITCHY_NATIVE_NAMES=1`, share of
samples in the parse):

| Function | Share | What it is |
|---|---|---|
| `bare_end` | 9% | past a bare key, a byte at a time |
| `value`, `one` | 15% | which kind of value starts here; a line |
| `probe`, `lookup`, `add`, `add_keyed`, `put` | 21% | the key index and a new node's four words |
| `skip_blank`, `skip_comment`, `end_line` | 8% | space and comments |
| `count_marks` | 5% | the pass that sizes the node array |
| `plain_end`, `scan_string`, `scan_key`, `key_path`, `take_key` | 13% | strings and keys |
| `Toml.push`, `Vec.push_spare_mut`, `witchy_cell`, `memory_init_unchecked`, `Toml.seal` | 14% | making the sealed document from the slices |
| `__bzero`, `memmove`, the rest | 15% | clearing the work and index arrays, the copy |

No function stands out. The reasons, in the order they cost:

1. **Small functions are calls.** The native backend inlines a leaf of at most 32
   instructions into a caller with at most 128 added. The reader is some ninety small
   functions (`peek`, `get`, `put`, `fail`, each scanner), and most of them stay calls
   that save and restore registers. Raising the limits to 64 and 512 gained 8% and was not
   kept: the gain a parser needs is inlining by profit, not a larger constant.
2. **A record comes back through memory.** A function that returns a record (`Made`, a
   `Node`) writes it to the caller's frame and the caller loads each field again
   (`Toml.text_of` copies a `Node` twice before testing one byte of it).
3. **The heap is checked on native.** Every `Vec` cell read or written in a function that
   is not proved goes through the runtime (`witchy_cell`, a range read). The reader avoids
   it by working in plain `U64` slices and pushing each finished node once, but that push
   is still four calls a node (14% of the parse), and toml-spanner writes into a bump arena.
4. **No vector kernels.** `U8x16` operations are library calls on native, slower than the
   loop they replace. The string scanner reads eight bytes at a time with `read_le64` and
   `U64.trailing_zeros`; bare keys, space and comments go a byte at a time.
5. **A proved sum keeps its test on native.** The Wasm emitter drops the overflow test of
   arithmetic a proof shows in range; the native emitter does not read that flag.

What was done, each measured on `zed`: reading into slices first and sealing after
(190 → 95 µs); eight bytes at a time in strings and UTF-8 validation with no vector calls
(95 → 70 µs); one fill for the work and index arrays in the runtime, where the fill was a
loop of calls (70 → 57 µs); one reservation for decoded text.

A later look at the emitted code found two things in the native backend itself: Cranelift
was run with its optimiser off (its default), so constants went unfolded and addresses were
computed twice, and a two-armed `match` was a jump table. With `opt_level` set to `speed`
and matches of up to four arms emitted as compares, the same run gives 51.3, 571.9, 15.7,
8.0 and 3396.6 µs: 3.0 to 3.3 times toml-spanner on the real files and 2.4 on the 1 MB file.
The hardware counters (`/usr/bin/time -l`) then say where the rest is. One parse of `zed`
retires 1.12 million instructions in 212,000 cycles: 61 instructions a byte, 1,450 a value,
at 5.3 instructions a cycle. Nothing stalls; the reader simply runs too many instructions,
and the profile above is a fair map of them. By construct, instructions a line:

| line | instructions |
|---|---|
| `k0000001 = 1` | 1,680 |
| the same with a 16-byte key, a 32-byte key | 1,970, 2,540 (36 a key byte) |
| `k0000001 = "abcdefgh"`, a 64-byte string | 1,580, 2,140 (10 a string byte) |
| a comment of 40 bytes | 760 (18 a byte) |
| `[t0000001]` | 1,220 |
| a string in an array | 1,000 |
| `k = { version = "1.0", features = ["a", "b"] }` | 6,140 |

For the plain `key = 1` line the profile gives about 480 to the key index (hash, probe,
node words), 260 to sealing the node, 300 to reading the `1`, 280 to the key, 210 to the
loop around them and 130 to `peek` as a call. Raising the inliner's limits a hundredfold
changes the time by nothing measurable, so the calls themselves are not it. What is left to
try, none of it done: every function of any size saves ten registers on entry and restores
them on leaving, twenty-odd times a line; key and comment bytes are classified with three
or four compares each; the node is written word by word through `put`, then again through
`push`. Parity is about 400 instructions for that line, a quarter of today's.

Three of the scanners were then put on eight bytes a step, as the string scanner is: bare
keys (a lane test for letters, digits, `-` and `_`), comments, and the key hash (a folded
product a word). `zed` goes from 1.12 to 1.08 million instructions and 51 to 49 µs
(3.0 times toml-spanner; `zed-lock` 2.9, the 1 MB file 2.25). The same change in two more
places measured 4% and 2%: the rest of the list above is of that size, an item at a time.

**Against Go** (2026-11-25, same machine and inputs, Go 1.27.3; `run.py --go go/toml-go`;
15 rounds, median, load 2.6 to 5): pelletier/go-toml v2.4.3 `Unmarshal` into a
`map[string]any`, the library's reading of a document, and BurntSushi/toml v1.6.0 `Decode`.
`go_ast` is go-toml's tokenizer alone (`unstable.Parser`, each top-level expression read): it
checks no duplicate key or redefined table and builds nothing to look a key up in, so it is
shown for scale, not as a reader of documents.

| input | witchy | go-toml | go_ast | BurntSushi | witchy / go-toml |
|---|---|---|---|---|---|
| zed | 49.4 µs | 119.8 µs | 19.8 µs | 463.2 µs | 0.41 |
| zed-lock | 564.0 µs | 695.6 µs | 254.6 µs | 4137.8 µs | 0.81 |
| extask | 16.3 µs | 27.4 µs | 6.2 µs | 161.3 µs | 0.59 |
| devsm | 7.9 µs | 8.6 µs | 2.9 µs | 57.0 µs | 0.92 |
| big | 3260.9 µs | 4186.2 µs | 1212.0 µs | 24038.1 µs | 0.78 |

Witchy reads every input faster than go-toml and 7 to 10 times faster than BurntSushi. The
margin is least on `devsm` (headers, dotted keys and inline tables: the key index and making
nodes), where what is left is the compiler's list above: four runtime calls to seal each node
(element invariants show one push at a time, not a loop), records such as `Made` returned
through memory, and calls that save every register. Under heavy load (an average of 27) one
run put `devsm` at 1.5 and `big` at 2.0; the rounds above are on a quiet machine.

**Allocations.** `scripts/toml-allocations.py` runs the benchmark program with
`WITCHY_COUNT_ALLOCATIONS=1`: one parse makes 7 allocations at 100 KB, at 1 MB and at 8 MB
(the benchmark's own copy of the text among them). On the inputs with no escaped string it
makes 5: that copy, the work array, the key index, the node array and the empty decoded
text; the first escape adds the decoded text's one reservation.

**Wrong answers.** The toml-test corpus (v2.2.0) passes for TOML 1.0 (679 cases) and 1.1
(681) on Wasm and native. `scripts/toml-fuzz.py` made 10,000 mutants of the corpus for each
of three seeds: none that Witchy reads and the `toml` crate refuses; two that the crate
reads and Witchy refuses, both a line end between `=` and a value in an inline table, which
the 1.1 grammar does not allow.

The independent review then found three wrong answers neither of those reaches, by
generating documents and comparing every value with Python's `tomllib`: `123e-5` was taken
for a date (the test for one looked for `-` after four bytes, not four digits); a table whose
key had an escape lost that key when a header later named it (the header rewrote all the
table's flags, the one saying where its key is among them); and a dotted key was refused
where it added to a table only named on the way to a header (`[a.b.c]`, `[a]`, `b.d = 1`).
Each is fixed with a direct fixture (`tests/fixtures/toml/found-in-review.wy`,
`state-implicit-dotted.wy`), and the comparison is kept as `scripts/toml-differential.py`:
30,000 generated documents, none read differently.

Fuzz seeds not run before (11 and 12) then found mutants the `toml` crate refuses and
Witchy read: digits too large for any finite number (`1e500`), read as infinity. They are
refused now, as an integer outside its range is, and only `inf` is infinity; eight seeds of
10,000 mutants have no wrong accept. Python's `tomllib` reads them as infinity, which the
differential now expects to be refused.

A second independent review, of the merged reader, tried about 6,000 hand-made cases (floats
at rounding boundaries, integer limits, dates, every string form and raw byte, key identity,
table structure, depth, 70,000 keys) and 82,000 mutants. It found one more: in a multi-line
string a `\` before a CR alone was taken for a line-ending backslash. Fixed, in the same
fixture. It also saw the dump program fall over on a dotted key of 20,000 parts; that is the
dump's own recursion, and the reader reads one of 100,000.
