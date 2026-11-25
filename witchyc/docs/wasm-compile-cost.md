# Compile cost

Measured 2026-11-15 on this machine with a release build, proofs cached
(`WITCHY_PROOF_CACHE`), best of three, as the whole process from outside.
Starting the process and loading the binary is about 7 ms of each number.

| Program | Lines with imports | Goals | Before | After |
|---|---|---|---|---|
| `tests/fixtures/arithmetic/ok.wy` | 31 | 0 | 13.1 ms | 7.2 ms |
| `tests/fixtures/programs/words.wy` (String, HashMap, console) | about 1,600 | 76 | 27.8 ms | 17 ms |
| `tests/fixtures/json/events.wy` | about 1,700 | 87 | 26.9 ms | 16.5 ms |
| `tests/fixtures/hashmap/numbers.wy` | about 1,100 | | 18.5 ms | 12 ms |

Inside the compiler, for the word-count program (`wasm-measure`):

| Phase | Before | After |
|---|---|---|
| Read the source and its imports | 1.0 ms | 1.2 ms |
| Parse | 0.7 ms | 0.7 ms |
| Type and ownership check | 0.9 ms | 1.0 ms |
| Generate proof statements | 4.0 ms | 4.0 ms |
| Plan concrete functions | 0.5 ms | 0.5 ms |
| Look the goals up in the proof cache | 4.9 ms | 0.7 ms |
| Emit Wasm | 0.2 ms | 0.2 ms |
| Write the module | 7.9 ms | 0.4 ms |

Two changes:

- The module is no longer synced to disk before the rename
  (`src/compiler/files.rs`). A reader still sees the old module or the whole new
  one. A module lost to a power failure is rebuilt.
- The proof cache is one file read once, not a file per goal
  (`docs/wasm-proof-cache.md`).

A sampling profile of 639 builds in one process found both: 45% of the time
was in `open`.

What is left is mostly generating the library's proof statements (72 KB of
text for 76 goals) on every build, only to hash them and find them cached. A
cache keyed on the source instead would skip that, and would need the
compiler's own identity in the key to stay sound while the compiler changes;
it is not done. Replacing the per-name text search in
`record_model::build_body` with one pass was tried and made no measurable
difference; it was reverted.

Uncached, the Vec library's 27 goals take about 1.75 s of Lean
(`docs/progress.md`); that is unchanged.

To repeat: build with `--release`, then time
`witchyc wasm-checked FILE -o OUT.wasm` with `WITCHY_WASM_READY_BINDING` and
`WITCHY_PROOF_CACHE` set; `witchyc wasm-measure` prints the phases.

## Only reached functions are emitted

Review finding F8 noted that every nongeneric function was emitted, called or not.
`src/compiler/instances.rs::plan` now starts from `main` and emits what it reaches.
Every other nongeneric body is still expanded once, so layout and storage refusals
do not depend on whether something calls the function, and those instances are
dropped before emission. They no longer count toward the 256-function limit.
Checking and proof goals are unchanged: they run on all source bodies before this step.

| Program | Module before | Module after |
| --- | --- | --- |
| `programs/words` | 15,472 bytes | 13,456 bytes |
| `console/hello` | 9,605 bytes | 7,097 bytes |

Build time did not move (17 ms and 15 ms, release compiler, cached proofs).

## After signed integers, views and result-field clauses (2026-11-15, `57fdc198`)

Release compiler, proofs cached, best of eleven runs each. The machine was busy, so
the medians ran 1 to 3 ms above these.

| Program | Time | Module |
| --- | --- | --- |
| `arithmetic/ok` | 6.6 ms | 1,373 bytes |
| `signed/proved` (14 proved I64 functions) | 8.9 ms | 2,620 bytes |
| `scalar/result-fields` | 7.2 ms | 1,332 bytes |
| `console/hello` | 15.3 ms | 7,150 bytes |
| `programs/words` | 16.8 ms | 12,126 bytes |
| `views/settings` (Span over String) | 16.9 ms | 11,041 bytes |
| `programs/ledger` | 19.3 ms | 17,585 bytes |
| `programs/formulas` (Span, I64, HashMap) | 19.3 ms | 23,648 bytes |
| `json/roundtrip` | 20.0 ms | 33,196 bytes |

`arithmetic/ok` and `programs/words` are where they were before these features
(7.2 ms and 17 ms above). Nothing here was optimized; the table is a check that the
new checks cost nothing measurable.

## At the end of 2026-11-15 (`ddbdc449`)

Release compiler, proofs cached, best of eleven runs each, load average about 3.

| Program | Time | Module |
| --- | --- | --- |
| `arithmetic/ok` | 7.4 ms | 1,373 bytes |
| `signed/proved` | 8.5 ms | 2,620 bytes |
| `console/hello` | 17.5 ms | 7,150 bytes |
| `programs/words` | 19.1 ms | 12,126 bytes |
| `programs/tape` | 19.2 ms | 11,250 bytes |
| `programs/matrix` | 18.9 ms | 11,321 bytes |
| `programs/ledger` | 20.4 ms | 17,585 bytes |
| `programs/formulas` | 22.1 ms | 23,648 bytes |
| `programs/anagrams` | 22.9 ms | 20,796 bytes |
| `json/roundtrip` | 21.5 ms | 33,218 bytes |

Programs that import the library are about 2 ms slower than in the table above;
`arithmetic/ok`, which imports nothing, is where it was. The modules are the same
size. `wasm-measure` on `console/hello` puts 4.4 ms of the 9.2 ms spent inside the
compiler in `vc_generation` (4.0 ms in the first table of this file): a cached
build still writes out every library obligation to look it up, and the library has
more of them than it had (the Vec methods added since, and the console and hash
functions now proved not to trap). That accounts for about 0.4 ms of the 2 ms; I
did not attribute the rest, and the machine was loaded for both tables. Reading the 44 KB of source is 1.3 ms, parsing 0.8 ms, the borrow and type
check 1.0 ms, emitting 0.1 ms.

Not done, and the next lever if this matters: skip writing the obligations of a
library file whose text and compiler have both been seen accepted. That keys the
cache on source text and on an identity for the compiler, which the cache does not
have today, so it is a change to what the cache trusts and was left alone.

## Proof statements on several threads (2026-11-15)

A cached build still writes out every proof statement to look it up, and that
was 4.7 ms of `console/hello`'s 10 ms in the process, most of it in the record
model. Each function's body statement depends only on the program, so:

- the record model builds its bodies on up to four threads, each taking the
  next body not yet taken (`src/compiler/parallel.rs`), and puts them back in order;
- the storage, memory and numeric models run on one more thread beside it;
- the record model no longer runs the memory model a second time to check its
  imported helpers; it is handed the one result.

The goals, their order and the first refusal are as before: every fixture's
obligations are still cache hits, so the statements are byte for byte the same.
Release build, cached proofs, best of eleven:

| Program | Before | After |
| --- | --- | --- |
| `arithmetic/ok` | 7.1 ms | 7.1 ms |
| `console/hello` | 17.6 ms | 14.6 to 15.7 ms |
| `programs/words` | 19.4 ms | 16.9 to 17.4 ms |
| `programs/anagrams` | 22.2 ms | 20.5 ms |
| `programs/ledger` | 20.8 ms | 19.6 ms |

The longest single body is `String.push_scalar` at about 2 ms: it branches four
ways on the scalar's size and twice more in its loop, and the record model
states each path. Moving that arithmetic into helpers with numeric contracts
was tried: the record model refuses calls to functions without a record or
memory contract (it states such a path as unreachable), so this waits for the
record model to import numeric contracts.

Tried and dropped: reading each text's words once instead of searching it for
every name in `record_model::finish`. The statements were identical and the
steady state about 20% faster, but a single build did not move.
