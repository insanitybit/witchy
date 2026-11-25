# Progress log

One line per landed step: what now works, what is proved, what is still trusted.
Newest last. Trusted throughout unless a line says otherwise: the ownership
checker, the translation of source to proof obligations, the allocator, layout
and the Wasm emitter. There is no termination proof.

- 2026-11-14 `d5d6d9bc` Every Vec and String function with an unchecked memory operation has a kernel-checked contract, `string_append` included. Weak: `utf8_valid` proves only that it reads; `string_push_scalar` claims nothing on failure.
- 2026-11-14 `bb66a61f` `T: Any` type parameters; Buffer cells may hold owners; fixed-capacity `Vec<String>` runs in Wasmtime. Element ownership is enforced by the checker and the release guard, not by proofs.
- 2026-11-14 `ebc53ae0` `sealed type`: construction, field writes and owner extraction only in the declaring file. `Vec` and `String` sealed. Not yet used by proofs.
- 2026-11-14 `c6eced9e` Records with several owners, opened whole with `let Name(a, b) = move value`. `VecPushError<String>` and `VecTaken<String>` run in Wasmtime.
- 2026-11-14 `3c016f70` `ed8fd152` `e9016630` Anonymous records `.{label: Type}` and type aliases. Checker only; no proof change. Proof-free build of a small generic program: about 10 ms.
- 2026-11-14 `1213f753` Type invariants on sealed types: `Vec` states `memory.prefix(data, length, capacity)`; assumed wherever a Vec arrives, kernel-checked wherever the library returns, lends back or passes one on (8 refusal controls rejected). Run-time header checks, `vec_well_formed` and the reserve tail scan removed; five fixtures run in Wasmtime. Trusted: checker's sealing rule and the invariant's translation. Vec Lean time 2.0-2.3 s under load (budget 2 s: next task); Vec+String 3.0 s; proof-free build 6 ms.
- 2026-11-14 `792604e4` `memory.take_unchecked`: a proof-carrying move out of a cell, for owning elements too. `vec_take_last` and `vec_take_last_mut` use it and lost their storage-failure branches; both run in Wasmtime with `Vec<String>`. Proved: bound and occupancy at each take (5 more refusal controls rejected, 13 in all). Trusted: the emitted Wasm for the operation. Vec Lean time 1.9-2.0 s (2.5 s once under load); Vec+String 3.1 s.
- 2026-11-14 `3828347e` Growable `Vec<String>`: `vec_reserve` and `vec_push` are `T: Any` and grow by moving (`memory_move_prefix`, `memory_release_empty`); `tests/fixtures/vec-string/grow.wy` grows 0 to 7 and runs in Wasmtime. Proved: each callee `requires` at its call site, the move loop over two exclusive buffers, the Vec invariant after growth (6 more refusal controls rejected, 19 in all). Trusted: as before. Vec Lean time 1.84 s; Vec+String 2.96 s. Still Copy-only: `vec_get`, `vec_close`, `vec_truncate`.
- 2026-11-14 In-place push and take state what they do: `vec_push_spare_mut` and `vec_take_last_mut` had `ensures true` and now say which cell was written or taken, how the length moved, that every other cell and the allocation are unchanged, and why they fail. Proved: both contracts (6 more refusal controls rejected, 24 in all). Trusted: as before. Vec Lean time 1.9-2.1 s at machine load 5, about 0.1 s more than before and at the 2 s limit; 0.7 s of it is import loading in the read-only helper, and five proof-script variants gave no saving. Restoring margin waits for the proof cache.
- 2026-11-14 `?` and refutable `let ... else`: both are a match whose success arm is the rest of the block, so ownership, Wasm and proofs needed no change. `tests/fixtures/try/chain.wy` chains scalar and String operations and runs in Wasmtime. Proved: `push_pair` (two `vec_push` calls joined by `?`) adds two elements; the wrong count is refused. Trusted: the rewrite itself (it is ordinary checked source afterwards). Found: a contract on a scalar function returning `Result` is refused, because proofs are still chosen by signature shape. Proof-free build 10 ms.
- 2026-11-14 Declared sum types: `type Shape:` with any number of variants, generic sums (`Option<T>`), sums inside sums, owners inside variants, and exhaustive `match` that names a missing variant. `Result` is now one instance of the same tables, layout and Wasm dispatch. `tests/fixtures/sum/shapes.wy` and `sum/owned.wy` (an `Option<String>` and a three-variant `Label`) run in Wasmtime. Proved: nothing new; the Vec proof text is byte-identical (73337 bytes). Trusted: sum layout and tag dispatch in the emitter. Gap: contracts cannot mention a declared sum, and a proved body cannot call a function returning one. Proof-free build 10 ms; Vec Lean 2.07-2.12 s under load (unchanged text).
- 2026-11-14 A proved function can lend its record to a proved `&mut` operation: `vec_take_last` is now a call to `vec_take_last_mut`, whose contract is all the caller's proof knows. Proved: the wrapper's full contract from the callee's clauses and the kept invariant; 4 controls on the wrapper are refused (25 in all). Trusted: as before. Measured: one-shot build importing Vec is 7.8 s, of which 4.7 s hashes the 2.8 GB Lean installation twice and 2.2 s is Lean; the helper runs Lean on one thread by construction (the same file takes 1.3 s with threads). Vec Lean 2.04-2.14 s at load 5: still at the limit, and the remaining levers (threads, installation size) are in the read-only helper and SDK.
- 2026-11-14 Methods: `impl` blocks on declared records and sums, generic blocks, `T: Any` blocks, `value.method(...)` with the receiver lent, moved or copied as the first parameter asks, and `Type.function(...)`. A method is the function `Type.method`, so contracts, proofs, generics and sealing apply unchanged. `tests/fixtures/methods/counter.wy` and `methods/vec.wy` run in Wasmtime. Proved: `Vec.push_pair`, a user-declared method on the library Vec with a contract. Trusted: the receiver rewrite (it produces ordinary checked calls). Not done: the library still exposes plain functions. Proof-free build 10 ms; Vec proof text unchanged.
- 2026-11-14 The Vec and String libraries are written as methods (`String.new`, `text.push_scalar(&allocator, 233)`, `Vec.new`, `names.push(&allocator, move name)`, `names.len()`, `names.take_last_mut()`), and `tests/fixtures/vec-string/grow.wy` uses the method form throughout, including through a `&mut Vec<String>` parameter. Proved: the same 30 goals with byte-identical proof text (75162 bytes), Lean 1.9-2.0 s; the 25 refusal controls are refused under the new names. Trusted: as before, plus the impl-lifting rewrite.
- 2026-11-14 Shared references are values: `Vec.at(index) -> Result<&T, U64>` reads an element in place, and a `Vec<String>` is walked without taking anything out (`tests/fixtures/references/elements.wy`). A reference keeps its source lent until its last use; writing, moving or `&mut`-lending the source before then is refused, as is returning a reference to something the function owns. Proved: `memory_ref_unchecked` and `Vec.at` (33 goals, 78499 bytes, Lean 1.96-2.01 s, at the limit); 28 refusal controls. Trusted: the lending rule in the checker (tested by 5 unit tests, not proved) and the address arithmetic for `memory.ref_unchecked`.
- 2026-11-14 `for`: `for i in a..b:` counts, and `for name in &names:` walks any type with an `at` method by reference; `tests/fixtures/for/count.wy` and `references/elements.wy` run in Wasmtime. It is a rewrite into `while` and `match`, so nothing new is trusted beyond the rewrite itself. Proved: nothing new. Not yet: `break`, invariants on `for`, by-value iteration.
- 2026-11-14 `Vec.get` and `Vec.at` call the unchecked operations directly, so the two one-line wrapper functions and the unused `readable` predicate are gone. Proved: 29 goals, 74721 bytes (smaller than before references were added), Lean 1.84-2.00 s; 28 refusal controls. Trusted: unchanged.
- 2026-11-14 Traits: `trait`, `impl Trait for Type` (records, sums, scalars, and generic types with their own bounds), and bounded type parameters (`<T: Any + Area>`). Calls resolve to direct calls when generics are instantiated. `tests/fixtures/traits/shapes.wy` runs in Wasmtime. Proved: nothing new; a function with a proof cannot call a trait method yet. Trusted: the conformance and bound checks and the call replacement in `instances.rs` (5 unit tests).
- 2026-11-14 Proof cache (`WITCHY_PROOF_CACHE`, `run-fixtures.py --proof-cache`): an obligation already accepted under the selected checker binding is not sent to Lean again. Warm build of a `Vec<String>` program: 8.5 s -> 0.05 s, identical Wasm; the 14-fixture set runs in seconds. Receipts mark each obligation `fresh_kernel_checked` or `cached_kernel_checked`. Proved: nothing new. Trusted, new and opt-in: the cache directory's integrity. Default builds are unchanged.
- 2026-11-14 HashMap: `library/wasm-hashmap.wy`, a generic open-addressing map (`new`, `insert`, `get` -> `&V`, `contains`, `remove`, `take_any`, `resize`, `release`) over keys with `Hash` and `Same`; `String` and `U64` implement both. Written with new general checked operations `memory.try_init` (hands the owner back) and `memory.ref`, plus `&place` as a value. `tests/fixtures/hashmap/{numbers,words}.wy` and `buffer/checked.wy` run in Wasmtime. Proved: nothing new, and nothing needed: the map uses no unchecked operation. Trusted: the checker and emitter paths of the two operations and of `&place` lending (3 unit tests); that `Hash` agrees with `Same` is unchecked but not a safety matter.
- 2026-11-14 Text literals and formatting: `"text"` is a `Text` value (`byte_len`, `byte_at`), generated as ordinary Witchy source with no data section; `library/wasm-format.wy` has the `Show` trait for `Text`, `String`, `U64`, `Bool`, appending to a `String` and leaving it unchanged on failure; `String.truncate` cuts at scalar boundaries. `tests/fixtures/text/literal.wy` and `format/show.wy` run in Wasmtime. Proved: nothing new; the formatter uses no unchecked operation. Trusted: nothing new (literals go through the ordinary checker). Not possible yet: printing a String (host has only `print_u64`).
- 2026-11-14 JSON: `library/wasm-json.wy`, a full-grammar event parser generic over a `JsonSink` trait (escapes, surrogate pairs, depth limit, error codes, no leaked Strings on error); `String.scalar_at`. `tests/fixtures/json/events.wy` runs in Wasmtime. Proved: nothing new; no unchecked operation. Trusted: nothing new. Missing for a JSON tree: sums stored in buffers.
- 2026-11-14 Stored and recursive types: a declared sum (and `Result`) is now a record field and a Buffer element, so `Vec<Shape>` and `Vec<Option<T>>` work through the proved Vec library unchanged; a sum or record may hold itself inside a Buffer (`type Json: Null / Number(U64) / Array(Vec<Json>)`). `tests/fixtures/sum/stored.wy` and `sum/tree.wy` run in Wasmtime. Proved: Vec's obligations are generic and were re-checked for the new element types; Vec library goal text unchanged (29 goals, 74721 bytes; 2.0-2.4 s measured with unrelated machine load near 5, 1.9-2.0 s earlier the same day). Trusted: the stored-sum layout and the deferred Buffer element layout in `concrete.rs` (5 unit tests).
- 2026-11-14 JSON tree: `library/wasm-json-tree.wy` parses into a recursive `Json` value (`Array(Vec<Json>)`, `Object(Vec<Member>)`) and `json_close` releases it; partial trees are closed on every error path. `tests/fixtures/json/tree.wy` runs in Wasmtime. Proved: nothing new (Vec's generic obligations cover `Vec<Json>` and `Vec<Member>`). Trusted: nothing new. Gap found: a tree is read only by consuming it; `match` through a reference is next.
- 2026-11-14 Reading sums in place: `&Sum` is a reference type and `match` on a shared reference binds references to the variants' values, so a tree is walked without taking it apart; `Json` implements `Show` and JSON text round-trips (`tests/fixtures/references/match.wy`, `json/roundtrip.wy`, `json/roundtrip-escapes.wy`). Proved: nothing new; a function with a proof cannot match through a reference. Trusted: the stored layout of a variant's value and the `MatchRef` emitter path (3 unit tests). Found: buffer operations cost time proportional to allocation size because the trusted allocator re-validates the occupancy map on each one; left as is and recorded as a question for the user.
- 2026-11-14 `break` and `continue` in `while` and `for`, including from inside `match` arms; `for` steps its hidden counter before the body. `tests/fixtures/loops/jumps.wy` runs in Wasmtime; the HashMap library uses it. Proved: nothing; refused in functions with a contract. Trusted: the ownership check at the jump (same rule as the loop body end) and the label depth in the emitter (1 unit test, 7 refusals). Vec Lean time reads 2.1-2.2 s with the machine at load 4-5 from other processes; goal text unchanged at 74721 bytes.
- 2026-11-14 Borrowing a value with no name: `double(&21)`, `area(&Square(5))`, `Point(3, 4).sum()`, `"ok".show(...)`, `42.show(...)`; a bare number receiver is a U64. `tests/fixtures/references/temporary.wy` runs in Wasmtime. Proved: nothing; refused in functions with a contract. Trusted: the hidden cell and its write-then-address order in the emitter (2 unit tests; refusals for `&mut` temporaries and owner temporaries).
- 2026-11-14 `_:` rest arm and `Variant(_)` in `match`, by value and through a reference; `&Variant` of a variant that holds nothing is a borrowed temporary. `tests/fixtures/match/rest.wy` runs in Wasmtime. Proved: nothing; refused in functions with a contract. Trusted: the checker's expansion of the rest arm per variant (2 unit tests; refusals for a lost owner, a rest arm that is not last, never taken, or given a binder).
- 2026-11-14 `Discard` trait (`library/wasm-discard.wy`): `Vec<String>`, `Vec<Vec<String>>` and `HashMap<String, Vec<String>>` are discarded in one call; `Json` and `Member` implement it, replacing four hand-written drain functions. `tests/fixtures/discard/words.wy` runs in Wasmtime; no compiler change. Proved: nothing new. Trusted: the ownership checker; that all memory goes back is tested, not proved.
- 2026-11-14 Took in the borrow safety fix (`6e923196`, from an independent review): a `&mut` argument into a sealed record's field now needs the same permission as an assignment, and a reference nested at any depth of a sum keeps its loan and cannot be returned from a local. References inside records, arrays, Buffers, and references to sums that hold references are refused. Proved: nothing new. Trusted: the checker; 11 regression tests.
- 2026-11-14 Buffer operations no longer cost time in proportion to the allocation: `validate` checks the extent's two ends, release walks it all. Vec push/pop of 200 went from 899,402 to 222,104 instructions and 800 now fits the budget (`tests/fixtures/scale/`); the host has a `--fuel` report. Proved: nothing; the Vec proofs are unchanged. Trusted: the allocator, with a narrower self-check (argument in `docs/wasm-runtime-cost.md`; 1 new allocator control).
- 2026-11-14 `Vec.close` empties only the cells in use: allocating and closing a 4000-cell Vec went from over the budget to 193,839 instructions. Proved: `memory_take_prefix` and `Vec.close` (release of an emptied buffer, using the Vec invariant); 4 new refusal controls, 32 in all. Trusted: unchanged. Vec library Lean time is now 1.99-2.11 s for 31 goals.
- 2026-11-14 An access checks the token against its entry and release audits the entry in full: `Vec.get` went from 276 to about 150 instructions, a push and pop pair from 690 to 440; 1600 pushes and pops and a 500-byte String now fit the budget. Raw-buffer helpers moved to `library/wasm-buffer.wy` (Vec library: 27 goals). Proved: nothing new; 32 refusal controls still refused. Trusted: the allocator, with the entry audit moved to release (my judgement, beyond what was asked; `docs/wasm-runtime-cost.md`).
- 2026-11-14 Anonymous sums: `.[Empty | TooBig(U64)]` types, `.Tag` values, dotted match arms, owners inside, storage in Vec, generics; a smaller sum widens into a larger one at arguments, returns, bindings and through `?` (`tests/fixtures/union/errors.wy`, `owners.wy`; `docs/wasm-unions.md`). Proved: nothing; refused in functions with a contract. Trusted: tag lookup, the widening test and the emitter's renumbering (6 unit tests with refusals for narrowing, payload change, unknown tag, missing type).
- 2026-11-15 Store confinement: allocator state lives in a second Wasm memory that only the allocator's six helpers may name, and the engine process checks that on the bytes it runs, independently of the compiler (`docs/wasm-store-confinement.md`). Compiled code reaches a cell only through the `cell` helper and no longer reads entries or writes the bitmap. Refused in controls: program stores and loads into allocator memory, helper stores into program memory, memory size/grow/fill/copy, wrong memory shapes. 34 fixtures run in Wasmtime on the new host; 197 unit tests; allocator controls 19 of 20 (one stale from before), typed-storage 19 of 19. Proved: nothing new; proof text unchanged. Trusted: the helper bodies (identified by position), Wasmtime and the host; the reduced access checks of `dc0d63a3`/`c6c5d15f` now rest on the scan and stay. Cost: about 23% more instructions per cell operation until shifts land.
- 2026-11-15 Sizes are a choice: `--heap-pages`, `--slots`, `--stack-bytes` at build; `--fuel`, `--memory-pages`, `--module-bytes`, `--deadline-ms` at run (`docs/wasm-memory-sizes.md`). The profile admits `i64` and/or/xor/shl/shr_u, and the engine's scan is now an allowlist of instructions. Allocator: shift-based cell masks, element size and count in the entry, search hints, word-wise occupancy scans. `tests/fixtures/scale/big.wy` pushes 50,000 onto a growing Vec and holds 300 Strings at once. Costs are below the pre-confinement numbers (Vec 200 push/pop 97,563; `Vec.new(4000)`+close 23,485). Controls: 23 allocator (a 4000-step model comparison, grant and size refusals, opcode refusals), 19 typed-storage, 35 fixtures, 200 unit tests. Proved: nothing new; proof text unchanged, Vec Lean 1.90-1.93 s. Trusted: the allocator helpers, now with hints (lower bounds only) in their own memory.
- 2026-11-15 Scalar proofs generalized (`docs/wasm-scalar-proofs.md`): a function of unsigned integers and Bools with variables, branches, loops, matches and calls is proved against its actual body; any function, of any shape and inside any control flow, is checked for the `requires` of the scalar contracts it calls. `tests/fixtures/scalar/proved.wy` (ten functions: max, clamp, counting and summing loops, nested loops, gcd, Bool results, U8) runs in Wasmtime. Proved: no arithmetic trap, loop invariants, every `ensures` at every return. Refused: twelve false claims in `tests/fixtures/scalar/*.refused.wy`. Trusted: the walk in `src/compiler/scalar_vc.rs`; partial correctness only (no termination). Vec Lean time unchanged (1.83 s, 27 goals). Not yet: clients that borrow a Vec and reason about its methods.
- 2026-11-15 Client proofs: a function that borrows a Vec states claims through `items.len()` and `old(items.len())`, and is proved from the contracts of the Vec methods it calls (`docs/wasm-scalar-proofs.md`). The walk follows scalar fields of records, behind references too, and takes the number comparisons out of memory and record contracts, per `Ok`/`Err` arm. `tests/fixtures/clients/vec.wy` runs in Wasmtime: `count_below` (every `get` in range), `last_index`, `push_two` (all or nothing), `drain` (ends empty), `has_first`. Refused: nine false claims beside it. Contracted functions over numbers may now use references, `break`/`continue`, anonymous sums and matches through references. Trusted: the walk, including which places a store or loan can change; the reading of a field-returning method as its field. Not proved: anything about element values.
- 2026-11-15 Clauses read fields of the result (`ensures result.low <= result.high`) and Bool fields as propositions (`ensures flag.on`), and callers use them (`tests/fixtures/scalar/result-fields.wy`, six refusals). Proved: each such clause, by the kernel, against the function's body. Trusted: the walk's reading of a returned record, in `src/compiler/scalar_vc.rs`, as before.
- 2026-11-15 Nested patterns: `Ok(Some(x))`, `Err(.Locked(by))`, to any depth, with `Variant(_)` and the outer `_:` as fallbacks (`docs/wasm-sums.md`). A rewrite to one match per level before checking, so ownership, references and exhaustiveness are the ordinary rules. `tests/fixtures/match/nested.wy` runs in Wasmtime (declared and anonymous sums, a String moved out through two levels, a match through a reference); a missing inner variant and a mixed `Ok(other)` are refused. Proved: nothing new. Trusted: the rewrite (it produces ordinary checked source).
- 2026-11-15 Review finding F5: `memory.frame_unchanged` is gone. It rendered `frame = frame`; contracts that use it are refused with that explanation, the three Vec clauses that did are deleted, and the unused `frame` binder is out of every generated statement. Proved: the same Vec obligations without the tautology. Trusted: unchanged; nothing is claimed about allocations a contract does not name.
- 2026-11-15 Review finding F6 (hygiene and dispatch): the functions that hold a text literal's bytes are named `#text_literal_N` after lexing, which no source can spell; a program's own type named `Text` is refused with a message instead of colliding; a literal is found by halving on its id and a word by halving on its offset, not by a linear chain. `tests/fixtures/text/names.wy` runs in Wasmtime. Still as the review says: `Text` is an ordinary record a program can construct with any id (an unknown id reads as empty), and bytes are constants in code, not a data segment.
- 2026-11-15 Review findings F7 (the cheap parts) and F9: the container of a `for` is evaluated once (`tests/fixtures/for/once.wy`); a method found in two of a type parameter's trait bounds is an error, not the first one; a second method of the same name on a type, from another trait, is refused with a message. Receipts: a sum reports whether it has a stored layout and its size; a record lists every owner field (`owner_fields`); a template is `structural-owning-template`; `generic_parameters` is read from the statement checked. README and `docs/wasm-cutover.md` no longer deny owner elements, the Vec invariant, stored sums or returned references. Not done from F7: trait identity kept through monomorphization, an iterator protocol with its own end marker, `?` inside expressions.
- 2026-11-15 Console text (`docs/wasm-console.md`): `console.print_byte(U8)` and `library/wasm-console.wy` (`write`, `line` for literals, `write_string` for a String). Works: `tests/fixtures/console/hello.wy` prints text through Wasmtime; a `U64` as a byte is refused. Proved: nothing new. Trusted: the engine's `print_u8` import, which takes the byte by value and reads no guest memory. Modules that print now import two functions, so hosts built earlier refuse them.
- 2026-11-15 Variants that hold several values (`docs/wasm-sums.md`): `Box(U64, U64)`, `Box(3, 4)`, `Box(width, height):` by value, through a reference, nested, in anonymous sums, with owners inside (`tests/fixtures/variants/several.wy`). A rewrite to an anonymous record with positions as labels, in the parser and beside the nested-pattern rewrite; no new layout or ownership rule. Refused: wrong number of names; `_` over an owner. Proved: nothing new. Trusted: nothing new.
- 2026-11-15 `?` inside values (`docs/wasm-results.md`): `var x = f()?`, `x = f()?`, arguments, returned values, matched values and `if` conditions. Refused: a `?` that would be moved ahead of an earlier call, or that sits on the right of `&&`/`||`. A word-count program runs end to end (`tests/fixtures/programs/words.wy`: literal, Strings, `HashMap<String, U64>`, walking the map, printed text); it needed `HashMap.entry(slot)` for walking a map in linear time and `Discard for Entry`, both library code. Proved: nothing new. Trusted: nothing new.
- 2026-11-15 Checked arithmetic is written in place instead of calling a helper (`docs/wasm-runtime-cost.md`): a 100,000,000-step counting loop went from 0.44 s to 0.05 s in Wasmtime. Works: all fixtures; new `tests/fixtures/arithmetic` shows each overflow still traps and the edges still compute. Proved: nothing new. Trusted: the emitter's in-place tests, in place of the helpers' identical ones.
- 2026-11-15 Small functions that call nothing are written into their callers (`src/compiler/inline.rs`, `docs/wasm-runtime-cost.md`): 100,000,000 calls of a small function went from 0.19 s to 0.07 s; a `Vec.get` loop from 0.17 s to 0.13 s through `Vec.len`. Writing in callees that call the allocator was measured, gave nothing and was dropped. Works: all fixtures, through the engine's confinement scan. Proved: nothing new. Trusted: the pass, with the emitter.
- 2026-11-15 Compile cost measured and cut (`docs/wasm-compile-cost.md`): a cached build of the word-count program went from 27.8 ms to 17 ms as a process (about 7 ms of that is starting the process). The output is no longer synced to disk before its rename, and the proof cache is one file read once instead of a file per goal. Works: all fixtures, with every cached goal still found. Proved: nothing new. Trusted: the cache directory, as before, when the operator names one.
- 2026-11-15 `memory.exchange(&mut a, &mut b)` (`docs/wasm-exchange.md`): two places exchange their values, owners included, so a value behind `&mut` can be replaced. With it, as library code: `Vec.push_mut` and `Vec.reserve_mut` grow a Vec where it is, `HashMap.put` grows the table. Also `*=`, `/=`, `%=`. Works: `tests/fixtures/exchange` (2 programs, 6 refusals); the word-count program starts from an empty map. Proved: the new functions' calls into proved Vec methods (call preconditions, kernel-checked); not what the new functions themselves do. Trusted: the checker's disjointness rule for the two places and the emitter's exchange sequence. Vec library: 31 goals, about 1.75 s.
- 2026-11-15 Fixed before it left the branch: `memory.exchange` did not ask for write authority, so code outside the Vec library could exchange two vectors' `length` fields. It now needs what an assignment needs (`tests/fixtures/exchange/field-of-sealed.refused.wy`, `lent.refused.wy`). The fixture runner compares a refusal with the diagnostic messages only, not the path or source line, which is how the gap hid.

## Reached-only emission (review F8, first part)

Works: a module holds only the functions `main` reaches; unused library functions
are checked and proved as before and then left out (`docs/wasm-compile-cost.md`).
All 65 fixtures and 208 unit tests pass. Proved and trusted sets are unchanged.
Remaining from F8: compact stored sums and stack slot reuse.

## String in place

Works: `String.push(&mut self, allocator, scalar)`, `push_text` and `push_string`
append in place and answer `false`, leaving the text as it was, when there is no
room or the scalar is not one (`tests/fixtures/string/in-place.wy`). The word-count
program uses them and lost its by-value threading. They are plain Witchy on Vec's
`&mut` functions, with no proofs of their own and nothing unchecked: room is
reserved first, then bytes go in through the proved `push_spare_mut`.
Not proved: that a String's bytes are UTF-8. That rests on reading `library/wasm-string.wy`,
which is the only file that can build a String (sealed type), as before.

## `match` and `if` as values, `else if`, calculator program

Works: `let x = match ...:` and `let x = if ...: ... else:` for scalars and owners,
nested, with `?` and with paths that leave; `else if`; a written type on the `let`
settles the type of a generic call being matched (`docs/wasm-choose.md`).
`tests/fixtures/programs/calc.wy` (tokenizer into `Vec<Token>`, recursive-descent
parser, six failure variants in an anonymous sum) runs in Wasmtime; it compiled
unchanged apart from these three additions. Seven refusal fixtures.
Proved and trusted sets unchanged. Available inside functions with contracts about
numbers (`tests/fixtures/choose/proved.wy`), not yet where the contract speaks of memory.
Gaps the program showed that remain: a record cannot hold a reference (the parser
state had to be two parameters); a refutable `let` cannot carry a type hint.

## Proofs for the in-place Vec methods

Works and proved (Lean kernel, from the functions' actual bodies):
- `Vec.push_mut`: `Ok` adds one to `len()`, `Err` leaves it. `Vec.reserve_mut`: `len()`
  unchanged, capacity never smaller, and at least `requested` when it answers true.
  Both are generic and use `memory.exchange`.
- `push_mut` no longer traps when doubling the capacity would leave U64: it answers `Err`.
  The proof required it (every `+` and `*` in a contracted function is shown in range).
- Clients prove claims from those contracts: `tests/fixtures/clients/in-place.wy`.
- General mechanisms added to the scalar proofs (`docs/wasm-scalar-proofs.md`): generic
  functions, `ensures match result:` arms, and `memory.exchange`.
- `Vec.capacity()` now returns the `capacity` field instead of asking the allocator, so
  clauses can speak of it and a call costs no allocator helper. The field equals the
  allocation's capacity by the type's proved invariant.
- Eight new refusal fixtures; the 23 older client and scalar proof fixtures are back in the
  routine run (my earlier change to the fixture runner had stopped matching their
  expected lines; the runner now reads `proof failed:` and `not proved:` lines too).
Cost: the Vec library's Lean time went from 1.84 s to 1.97 s (33 goals, measured back to
back under the same machine load; about 1.75 s to 1.88 s when the machine is quiet).
Trusted, unchanged in kind: the walk in `src/compiler/scalar_vc.rs`, which grew by the
exchange rule and the per-variant return rule.

## Arguments that read what the call borrows

Works: `items.push_mut(allocator, items.len() * 10)`, `add(&mut counter, add(&mut counter, 2))`.
A reference argument that runs nothing is checked after the other arguments
(`docs/wasm-borrow-order.md`); the emitted order is unchanged. Still refused, with
fixtures: an argument that consumes the borrowed value, two references to one value,
a kept reference across the call. Not applied inside functions proved by a memory or
record model. Nothing new trusted.

## Unmetered runs; chosen values after `return` and assignments

- Measured: the engine's instruction count costs about a quarter of run time
  (1,000,000 pushes and 20,000,000 `Vec.get`: 0.15 s counted, 0.11 s not).
  `--unmetered` (launcher and engine) runs without it, bounded by the deadline,
  the memory grant and the stack; the default is unchanged
  (`docs/wasm-memory-sizes.md`, `tests/fixtures/scale/unmetered.wy`). Controls run
  by hand: a loop that never ends exits with status 124 at its 300 ms deadline;
  `--unmetered` with `--budget` or `--fuel` is refused.
- Measured, not changed: a push costs about 450 counted instructions and 20 ns with
  room reserved, 810 and 35 ns when the vector grows from empty; about 40% is the
  allocator's access helper, which dispatches on the operation at run time. Reading
  `capacity()` from the field took 7% off the growing case.
- `return match`, `x = match`, `x += if ...` (`docs/wasm-choose.md`).
What remains trusted: unchanged. An unmetered run trusts the deadline thread and
the launcher's timer to stop a program, where a counted run trusts the count.

## Console input

Works: `fn main(console: Console<Write>, input: Console<Read>, allocator: Allocator)`;
`input.next_byte()`; library `read_byte` and `read_line` (UTF-8 checked, one reused
String); `String.clear`, `Vec.truncate_mut`. `tests/fixtures/programs/sums.wy` reads
a file of numbers through Wasmtime. The engine and launcher take `--input=FILE` and
`--input-bytes=N`; the file is read whole before the run (`docs/wasm-console.md`).
A program that imports the console library and reads nothing imports no input
function. Five refusal fixtures cover the grant.
Proved: nothing new. `Vec.truncate_mut` has no contract; the Vec library is 34 goals,
about 1.98 s on this loaded machine, the same as before the change.
Trusted, new: the engine's `read_u8` body and its reading of the input file. The host
still reads and writes no guest memory.

## Vec elements in place; larger programs

- `Vec.replace_mut(index, item) -> Result<T, T>` puts an item where an element was
  and hands the element back; any element type, owners included. Proved against its
  contract (length and capacity unchanged, the old cell's value returned, the new
  value stored, every other cell unchanged) and the Vec invariant. Five false
  variants are refused.
- `Vec.set(index, item)` and `Vec.swap(first, second)` (each a `Result<(), U64>` since 2026-11-22; a `Bool` before) for copied
  values, written in safe code on `get` and `replace_mut`; no contract.
- `tests/fixtures/programs/sort.wy`: insertion sort of a `Vec<U64>`, and a String
  exchanged inside a `Vec<String>`.
- The memory proof accepts a result `Result<T, T>` (it accepted `Result<T, U64>`
  and `Result<(), T>`).
- The Vec refusal controls are in the repository: `scripts/check-vec-refusals.py`
  (37 cases, about four minutes; the last five are new).
- Limits on program size raised: `docs/wasm-memory-sizes.md`, "Program size".
- Vec library proof time: 39 goals, 2.08 to 2.12 s on this machine under load,
  against 1.96 to 2.01 s for the previous 33 goals measured the same minute. That
  is about 0.11 s more, and over 2 s as measured here.
Not done: a swap for owners. Proved directly (two takes, two stores) it added
0.5 s to the library's proof time, so it was left out.
Trusted: nothing new.

## `&mut` references as values

Works: a function returns `&mut T` or a variant holding one; `let`, `match` binders,
passing on by name, `&mut place` as a value, `memory.mut`; `HashMap.get_mut`. The
word-count program now adds one to a count where it is stored, instead of removing
the entry and putting it back. Rules and refusals: `docs/wasm-references.md`,
"Unique references". Fixtures: `tests/fixtures/references/unique*.wy`; unit tests:
`src/compiler/unique_reference_tests.rs` (30 programs, 21 refusals).
Proved: nothing; functions with a contract are refused the feature.
Trusted: the borrow rule in the checker, as for `&T`. Three attempts to reach a
sealed type's field through a kept `&mut` inside a copy of the Vec library were
refused (by hand).
Not done: `Vec.at_mut`, and matching through a `&mut` reference.

## `Vec.at_mut`

Works: `Vec.at_mut(index) -> Result<&mut T, U64>`; elements of any type are changed
where they are, including a String inside a `Vec<String>`
(`tests/fixtures/references/elements-mut.wy`).
Proved: `Vec.at_mut` against its contract and the Vec invariant, with the element
treated as an arbitrary value from the moment the reference is made
(`docs/wasm-references.md`). Six false variants refused; a type whose invariant
depends on the element is refused the `&mut` form and allowed the `&` form.
The Vec refusal set is 43 cases.
Trusted: the model rule for `memory.mut_unchecked`; the borrow rule that keeps the
vector lent while the reference is used.
Vec library proof time: 40 goals, 2.07 to 2.19 s on this machine under load.

## `for ... in &mut`, String operations, `?` on a call with calls inside

- `for item in &mut container:` walks with `at_mut` and gives `&mut` references
  (`docs/wasm-iteration.md`, `tests/fixtures/for/change.wy`). Any type with an
  `at_mut` method; nothing names `Vec`.
- `library/wasm-text.wy`: compare, order, find, slice, parse and print numbers
  (`docs/wasm-string.md`, "Text operations"). `String.cut` in place.
- `tests/fixtures/programs/frequency.wy`: word frequencies of an input file.
- Fixed: `text(allocator, "literal")?` and `half(twice(n))?` were refused as "this `?`
  runs after an earlier call". Calls inside the operand of `?` move with it; only a
  call before the operand is refused, as before (`tests/fixtures/try/operand-calls.wy`).
Proved and trusted: unchanged by these.


## Allocation in a fixed number of steps per slot

Works: allocating takes the newest released slot or the next unused one, so its
cost no longer grows with the number of live allocations. Word counting over
1 MB of text went from about 0.26 s to 0.10 s; building and closing a short
String with 2000 others alive from about 44,000 instructions to 2,400.
`tests/fixtures/scale/churn.wy` checks that Strings kept alive through 5000
others being made and closed still hold what was written.

Proved: nothing new. The library's proofs do not mention slots.

Trusted: the allocator's helpers, as before, now with the slot list and its
two words (`docs/wasm-memory-sizes.md`). The engine's confinement check is what
keeps program code from writing them. `scripts/test-wasm-allocator.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) models
the list over 4000 random steps; all its controls pass.

Measured, release compiler with cached proofs: word count 20 ms, sort 17 ms,
JSON tree 21 ms, 300 functions 14 ms, of which about 7 ms is process start.
Discarding a `HashMap` visits each cell once (it restarted from the first cell
for every entry): a 10,000-entry map with 200,000 lookups went from 16.9 G
instructions to 143 M. Still slow: the search for heap room is first fit from
a lower bound.

## Fields through a reference; shortest paths

Works: `pair.left`, `heap.items.len()` and `cells[i]` through `&` and `&mut`
without writing `(*pair)`. `tests/fixtures/programs/paths.wy` is Dijkstra's
algorithm with a binary heap in a record, adjacency lists in a
`Vec<Vec<Edge>>` reached with `at_mut`, and `for edge in edges`; it compiled
as first written once this was in, apart from one `impl Discard for Edge`.

Proved: nothing new. Trusted: the rewrite in the checker
(`docs/wasm-references.md`); fourteen programs compile to the same bytes as
their long forms or are refused alike.

The short form also works in a function proved by the memory model, which
reads the checked body.

## Contracts that name fields

Works: `requires pair.left < 100`, `ensures pair.left == old(pair.left) + 1`,
`ensures *value == old(*value) + 1`, invariants over fields, and callers that
lend part of a record and reason from the callee's clauses about that part.
`tests/fixtures/programs/ledger.wy` (commands read from input into a
`HashMap<String, U64>`) is a second new program; it needed nothing new.

Proved: each such `ensures` and invariant, by Lean, from the function's body.
Twelve false claims in `tests/fixtures/fields/` are refused.

Trusted: the scalar walk, as before (`docs/wasm-scalar-proofs.md`). The new
clause form resolves to the key the walk already used for `value.method()`.

Array elements at literal indexes came 2026-11-18 (`requires xs[0] < 1000`,
`scalar/array-*.wy`), as did `result.left` and Bool fields earlier. Not yet: a computed index
in a clause. Records
cannot hold references, so a cursor carries a position and is handed the text.

## `match` through `&mut`

Works: a sum behind a `&mut` reference is matched where it is and each arm's
name is a `&mut` reference to its variant's value. `add(&mut doc, ...)` appends
to the String or pushes onto the Vec a `Doc` owns
(`tests/fixtures/match/through-mut.wy`).

Proved: nothing. Trusted: the checker rule that keeps the sum lent while a
name into it is in use (`docs/wasm-references.md`); nineteen programs test it.

Two names in one arm came 2026-11-18 (`match/through-mut-two.wy`), once a reborrow through
a `&mut` local took the loan of the part reached rather than of the whole local. The same day a
function proved by the scalar walk may hold a `&mut` in a local, an arm's names included
(`references/proved-*.wy`). Not yet: in a function proved by the memory or record model.

## `I64`

Signed 64-bit numbers: literals, `-x`, `+ - * / %`, `+=`, `-=`, comparisons,
fields and elements, and `library/wasm-int.wy` for checked conversions, text
and map keys (`docs/wasm-signed.md`). Every operation gives the mathematical
result or traps; `scripts/check-i64.py` checks 2880 edge cases against Python.

Proved: nothing about signed values. No clause can name an `I64`, and to a
scalar proof an `I64` result is unknown, so nothing false follows from one;
four false claims are refused, and an unsigned contract beside `I64` code
proves.

Trusted: the emitter's overflow tests; six more admitted instructions
(`i64.lt_s gt_s le_s ge_s div_s rem_s`), none of which touches memory.

Compound assignment (`*= += -= /= %=`) works as the operators do (`signed/compound-assign.wy`,
`multiply-assign.traps.wy`); contracts over signed values came later (below). Not yet: other
widths, `I64` in functions proved by the memory or record model.

## Records that hold references

A record field may be a `&` reference. The record is then a view: Copy, passed
and returned by value, never stored, and what it points at stays lent while the
view or anything made from it is in use (`docs/wasm-views.md`).
`library/wasm-span.wy` is part of a `String` with `part`, `split`, `trim`,
`find_byte`, `equals_text`, `same`, `parse_u64` and `to_string`, none of which
copies; `tests/fixtures/views/settings.wy` reads settings out of one String
with it, and `slice.wy` has a generic `Slice<T>` over a `Vec<T>`.

Proved: nothing. No proof reads a view, and a function with a contract cannot
use one.

Trusted: the checker rules for views, tested by 68 programs in
`src/compiler/view_tests.rs` and eleven fixtures. One hole was found and closed
while testing (opening a view lost its loans).

Lending a view through `&` to generic code came 2026-11-18: a view may be a `T: Any` that the
callee holds only as `&T` (every parameter that names `T` is exactly `&T`, and the result does
not name it), so `map.get(&span)`, `map.contains(&span)` and `map.get_mut(&span)` work with
`Hash` and `Equivalent<String>` for Span (`hashmap/span-lookup-methods.wy`,
`views/lent-*.wy`). Not yet: `&mut` fields, `view.reference.field`, views as type arguments
otherwise.
(`for b in span` came 2026-11-18: `views/for-span.wy`.)

## A formula evaluator

`tests/fixtures/programs/formulas.wy` reads lines such as `y = (x - 5) * (x + 5) / 3`
and `print -big - 1` from input: a recursive-descent parser whose position is a
`Span`, signed values with the checked operations so that overflow and division
by zero are reported instead of trapping, and names in a `HashMap<String, I64>`.
It is 236 lines of safe code and needed no compiler change. A `match` on
`names.get(&key)` lends `key` only in the arm that holds the reference, so
the `Err` arm closes the key directly.

## Proofs about I64 values

Scalar contracts speak of I64 values, read as integers: clauses with negative
literals and `+ - *`, loop invariants, fields, callee contracts
(`docs/wasm-signed.md`). A function with a contract now shows that its I64
arithmetic cannot trap, as it already did for unsigned arithmetic.

Proved: fourteen functions in `tests/fixtures/signed/proved.wy`. Refused:
fourteen false or unsafe claims.

Trusted: the walk's reading of the I64 operations, tested against the emitted
code by `scripts/check-i64.py`.

The value of `/` and `%` came 2026-11-18: Lean's `Int.tdiv` and `Int.tmod`, which round
toward zero as `i64.div_s` and `rem_s` do (`signed/divide-*.wy`, `remainder-*.wy`). `auto`
proves exact values; a bound such as `x / 2 <= 0` for negative `x` it does not yet, since
`omega` reads only floor division. `I64.bits` and `I64.from_bits` came the same day: two's complement over 64 bits, written
with `emod` and `Int.toNat` so `omega` reads it (`signed/bits-*.wy`, `from-bits-*.wy`).
Not yet: I64 in memory or record proofs.

## A stack machine, and what writing it found

`tests/fixtures/programs/machine.wy` (about 200 lines) runs a `Vec<Op>` program on a
`Vec<I64>` stack: variants that hold values, an anonymous sum of faults with one and
two values, `?` throughout, and a ten-factorial program plus four that fault. It
prints `1001 2002 3040 5000 3628800 117`.

Writing it found, and this adds:

- `A | B:` in a match (`docs/wasm-sums.md`).
- `while condition?:` (`docs/wasm-results.md`).
- `Variant(_, _)` matched by value.
- `value?` alone when the value owns nothing (`tests/fixtures/try/unused.wy`); an
  owner still needs a `let` (`unused-owner.refused.wy`).
- A call to a method the console does not have says so, instead of "unsupported call
  or missing output authority".
- The engine names the trap. The first version looped; the only report was a
  backtrace, and the cause was the instruction budget.

Found and left:

- An array literal is limited to 32 scalar slots, so sixteen `Op` values cannot be
  written as one array; the program pushes them one by one. The limit is the
  flattened layout's (`src/compiler/layout.rs`), shared with parameters and results.
- `for item in array:` was refused; it is accepted now (`docs/wasm-iteration.md`).

Nothing here is proved or newly trusted: these are parser and checker rewrites into
forms the checker already had. The `_` change is in the trusted borrow checker and
is tested, not proved.

## A report, and sorting

`tests/fixtures/programs/report.wy` reads `name,department,salary` lines, keeps a
`HashMap<String, Stats>` updated through `get_mut`, sorts the department names and
prints a table with aligned columns. Bad lines are counted, not fatal.

What it needed, now in the library or the checker:

- `library/wasm-sort.wy`: a `Before` trait (U64, I64, String), `sort` (heap sort, in
  place, nothing allocated), `is_sorted`, `precedes`. Safe code on Vec's methods.
- `Vec.exchange(first, second)` for any element type, owners included. It holds the
  last element while the two change places, using `take_last_mut`, `at_mut`,
  `memory.exchange` and `push_spare_mut`; no unchecked operation and no new proof.
- `write_u64`, `write_u64_right`, `write_string_left`, `write_spaces` in
  `library/wasm-console.wy`.
- A reference or view chosen by `match` or `if` (`docs/wasm-views.md`).

Measured: sorting 1,000 numbers took 15.1 million instructions with a sink that
exchanged at every level. Holding the sinking element in hand, so that a level moves
two elements instead of three (`sort_sink_front`), brought that to 10.4 million.
`tests/fixtures/programs/sort-check.wy` checks 400 small inputs and one of 1,000
for order, length, sum and sum of squares. Each comparison is still two `at` calls
and each move an `at_mut`, all through the allocator's access helper. That is the same per-access cost recorded in
`docs/wasm-runtime-cost.md`; a sort is where it shows most.

Proved: nothing new. Trusted: the chosen-reference rule joins the borrow checker's
tested rules.

## `for` over arrays, and `for` in proofs

`for item in array:` works (`tests/fixtures/for/array.wy`). A `for` takes invariants
and is proved like a `while` (`tests/fixtures/scalar/for-loops.wy`, four refusals);
a function with a contract can now walk a Vec with `for`. Proved: each claim in that
fixture, by the kernel, against the rewritten loop. Trusted: nothing new.

## `break` and `continue` in proved loops

A loop with invariants may `break` and `continue` (`docs/wasm-scalar-proofs.md`,
`tests/fixtures/scalar/jumps.wy`, three refusals). Proved: the fixture's claims, by
the kernel. Trusted: the walk's rule for what is known after such a loop, in
`src/compiler/scalar_vc.rs`, with the rest of the walk.

## Library arithmetic proved not to trap

`U64.hash`, `I64.hash`, `String.hash`, `String.parse_u64`, `write_u64` and
`write_u64_right` carry `ensures true` and are proved (`docs/wasm-scalar-proofs.md`). Proved: none of their arithmetic can
overflow, go below zero or divide by zero, by the kernel. Not proved: what they
compute. Trusted: the scalar walk, as before.

## Life

`tests/fixtures/programs/life.wy`: Conway's Life on a wrapping grid held in a
`Vec<Bool>`, two grids exchanged each generation with `memory.exchange`. It compiled
as first written except for one line that added an I64 offset to a U64 index; that
refusal now says what the mismatch is and how to convert. Eight generations of a
12 by 8 grid take 2.26 million instructions, about 370 for each neighbour looked at,
most of it `Vec.get` through the allocator's access helper. The engine's default
budget is one million instructions, so this program, the sort tests and the stack
machine's looping case all need `--fuel`; the default is a choice for the user.

## A priority queue

`library/wasm-heap.wy`: `Heap<T: Before>`, a sealed generic type over a Vec, with
`push`, `pop`, `first` (a reference), `len`, `is_empty`, `into_vec` and `Discard`.
Safe code; elements may be owners. `tests/fixtures/programs/queue.wy` takes 200
events in time order and a heap of Strings in byte order. The shortest-path program
(`programs/paths.wy`) now uses it in place of its own heap and is fifty lines
shorter, with the same answers. Nothing is proved about the heap's order; `Before`
is trusted to be an order, as `Hash` and `Same` are trusted to agree.

`Vec.filled(allocator, count, item)` makes a Vec of copies
(`tests/fixtures/programs/histogram.wy`); a fixed array still holds at most 32
scalar slots, so a table is a Vec. The Vec library is now 44 goals and about 2.2 s
of Lean on a first build (40 goals and 2.15 s this morning): each safe method added
to `wasm-vec.wy` is one more goal for the calls it makes.

## A tape machine, and what writing it found

`tests/fixtures/programs/tape.wy` reads a program for an eight-command tape
machine from the input, pairs its brackets with a stack, and runs it. Writing it
found four things, each now in the language as a general rule:

- `'a'` is the number of a scalar, so a byte is compared with `'['`, not 91.
- `match` on a number, with arms like `'+' | '-':` or `'0'..='9':` and a last `_` arm. The parser
  reads it as the `if` chain, so proofs and the checker see nothing new.
- `impl Name<U64>:` gives methods to one instance of a generic type. The console
  library uses it, so text goes out as `console.line("…")` and
  `console.write_u64(n)`; the free functions are gone and every caller changed.
- `for index, item in container:` names each element's position.

Nothing proved or trusted changed: all four are gone before the borrow checker
and the proof models see the program, except the one-instance `impl`, which
produces ordinary functions. Refusals for each are fixtures beside the accepting
ones (`match/numbers-*`, `methods/instance-*`, `for/positions-*`).

A probe of the rule that sends read-only functions to the numeric proof: no form
of read-only method that reaches an unchecked memory operation is admitted
(`scalar/no-trap-unchecked.refused.wy`, docs/decisions.md).

Still awkward in that program: `?` does not carry a `U64` error into a function
whose error is a declared sum, and `match Vec.filled(…)` needs the type written
on a `let` first. (Later: a type written on the `var` that takes the `match` is
enough.)

## A matrix and anagram groups

`tests/fixtures/programs/matrix.wy`: a sealed `Matrix` over `Vec<Vec<U64>>`,
multiplied and printed. It was refused at `self.rows.at_mut(row)` in the type's
own method; a sealed type with no invariant now lends its fields in its own file
(docs/decisions.md). A type with an invariant still lends none, and no file lends
another's sealed fields: both are fixtures. This is a borrow-checker rule, tested
and not proved.

`tests/fixtures/programs/anagrams.wy`: words from the input grouped in a
`HashMap<String, Vec<String>>` under their sorted letters. It needed no compiler
change, and showed two places where the borrow checker is coarser than it could
be. Both refuse safe programs; neither admits an unsafe one:

- A reference returned by a call kept every reference argument lent, so after
  `groups.get_mut(&name)` the key `name` could not be closed while the group was
  in use. Fixed for the case that matters most: a result whose only references
  are `&mut` keeps what the call borrowed with `&mut` and frees what it borrowed
  with `&` (docs/wasm-references.md). A `&` result still keeps every argument;
  telling those apart needs a per-function summary checked against the body.
- Inside `for item in &items:` the container stayed lent through any statement
  that used `item`, so `if *item == 7: items.close(); return` was refused though
  the branch no longer uses `item`. Fixed: a reference last used in an `if`
  condition, or as the value of a `match`, is not in use in the branches or
  arms (docs/wasm-references.md). An `else if` that reads the reference is
  still coarse.

## Walking a String

`for scalar in &text:` and `for position, scalar in &text:` walk a String's
scalars through a second protocol, `next(&mut cursor)`, for values that are
decoded and not stored (docs/wasm-iteration.md, review F7 in part). Any type
with `next` is walked this way. Nothing proved or trusted changed.

`tests/fixtures/programs/bank.wy`, written after these changes, compiled and ran
as first written: commands parsed from input lines, three anonymous error sets
widened through `?` and `return`, and a key closed while the `&mut` balance found
under it is still in use.

`String.field(allocator, separator, &mut cursor)` takes a text apart one field
per call (`tests/fixtures/text-ops/fields.wy`); the bank program uses it. Safe
code in `wasm-text.wy`. Signed output is `console.write_i64` and
`console.print_i64`, methods like the rest of the console library.

`tests/fixtures/programs/generic.wy`, the last program written, is a program's
own sealed generic `Stack<T>`, its own trait, and generic functions bounded by
that trait and the library's `Before`. It needed no change to the compiler.

## Handoff, 2026-11-15, end of the twelve hours

Branch `work/vec-string-core`, 79 commits from `4e41cc7b` to this one. The main
checkout was checked after every commit and is as it was (25 status entries).

How to check it (none of this is a full gate):

- `scripts/run-fixtures.py` over `tests/fixtures`: 296 fixtures, each compiled
  and either run in Wasmtime against its `.expected` or refused with the stated
  message. Every refusal named in the docs is one of them.
- Rust unit tests, `--lib`: 235 pass, 1 ignored.
- `scripts/test-wasm-allocator.mjs`: 23 pass, 0 fail, 1 skipped.
- `scripts/check-i64.py`: 2304 exact, 576 trapped, 0 wrong.
- Compile times: `docs/wasm-compile-cost.md`, last table. 7 ms for a program
  with no imports, 17 to 23 ms for programs on the library, proofs cached.
- With no proof cache, `programs/bank`, `programs/anagrams` and
  `console/signed` were proved afresh at the end and accepted. That first build
  is slow: 7 to 9 s for a program on the library (release compiler, load
  average 4 to 6), of which Lean is 3.6 s over four invocations for 98 to 116
  goals. The cache is opt-in and joins the trusted base when used
  (docs/wasm-proof-cache.md).

Against the five parts of the goal:

1. Store confinement. Allocator state is in its own Wasm memory and the engine
   checks, by allowlist, that program code never addresses it
   (docs/wasm-store-confinement.md). The reduced access checks stay and rest on
   that scan. Trusted: the scan, the six allocator helper bodies, Wasmtime, the
   host.
2. Sizes. Heap, slots and fuel are flags; `--unmetered` exists; allocation is a
   fixed number of steps per slot from a released-slot list; shifts and signed
   comparisons and division are admitted (docs/wasm-memory-sizes.md,
   docs/wasm-signed.md). The default budget is still one million instructions.
3. Proofs. Scalar functions with variables, loops, `for`, `break`, `continue`,
   `I64`, result fields and Bool fields; client functions that borrow a Vec and
   call its proved methods; library arithmetic proved not to trap
   (docs/wasm-scalar-proofs.md). Each has refusals for false claims. Trusted:
   the walk that turns a body into obligations (`src/compiler/scalar_vc.rs`) and
   the memory and record models. Proofs are of partial correctness.
4. Review findings. F1, F2, F5, F6 (names and dispatch), F9, the cheap parts of
   F7, and reached-only emission from F8 are done. Open: F6's data segment, a
   trait for iteration (F7), compact stored sums and stack slot reuse (F8), and
   F10, which is a statement of what the opt-in proof cache trusts. Language:
   anonymous sums and widening, nested patterns, multiple-value variants,
   views, `&mut` references as values, `I64`, quoted scalars, hexadecimal
   numbers, `match` on
   numbers, one-instance `impl`, `for` with positions and over values, and
   String, HashMap, Heap, sort, text and console libraries. Twenty programs
   under `tests/fixtures/programs` use them.
5. Cost. Measured in `docs/wasm-runtime-cost.md` and
   `docs/wasm-compile-cost.md`. Checked arithmetic in place, leaf inlining,
   the allocator's free list and the sort's sink were kept because the numbers
   moved; three attempts that did not move them were dropped.

Decisions left for the user, each recorded where it came up in
docs/decisions.md:

- The allocator's per-access helper (about 80 instructions; `Vec.get` in a loop
  is about 127). Validating once per borrow changes the trusted allocator
  interface. (Done for proved reads afterwards; see the last section.)
- Proving only the functions a program reaches, which would cut the 4 ms a
  cached build spends writing out library obligations.
- The engine's default budget of one million instructions, which the larger
  programs exceed and set with `--fuel`.
- Vec's first-build Lean time is about 2.2 s for 44 goals, above the earlier
  2 s target: each safe method added to `wasm-vec.wy` is one more goal.

Judgement calls made today that narrow a refusal in the borrow checker, which
is trusted and tested, not proved. Each has fixtures on both sides and its
argument in docs/wasm-references.md or docs/wasm-views.md; they are the changes
most worth a second reading:

- a sealed type with no invariant lends its fields in its own file;
- a call whose result holds only `&mut` references frees what it borrowed with `&`;
- a reference last used in an `if` condition or as a matched value is free in
  the branches or arms;
- `let x = match/if` may give a reference or a view;
- records may hold `&` references.

Known limits: arrays and aggregates hold at most 32 scalar slots; a function
with a contract cannot hold a `&mut` reference or a view; a view cannot be a
type argument or hold `&mut`; `view.reference.field` needs a `let`; `?` does
not carry a `U64` error into a declared sum; a `&` result keeps every
reference argument lent; `for` cannot walk a HashMap; no termination proofs.

## After the handoff: what a result can point into

Carried on at the user's word. A call's reference arguments now stay lent
only if the result could point into them, judged by type on the function's
signature (docs/wasm-references.md, last section). `map.get(&key)` frees the
key, a function returning `&` may take a `&mut` cursor, and `for entry in
&map:` walks a HashMap. Tested on both sides (`references/points-*`,
`hashmap/walk*`), not proved; it is the sixth narrowing of the borrow checker
and the one with the longest argument, so it is the first to read.

## Proved reads without the allocator helper

- A token carries its allocation's first quantum, and a read a proof admits
  computes its address inline. `Vec.get` and `for item in &vec` run at the
  speed of a bare loop (0.134 s to 0.052 s for 2e7 reads).
- Proved: as before, that the index is in range and the cell full.
- No longer checked at run time on those reads: liveness, generation and type
  of the token. Trusted instead: the borrow checker (a borrowed Buffer is not
  released; a token is not forged or copied), the emitter's `proved_cell`, and
  the allocator's `first` field, which every checked operation compares with
  the token. Stale reads, if that trust failed, stay inside memory 0.
- Decided by me, recorded in docs/decisions.md with how to revert.

## `unreachable`

- A statement that stops the run with a trap; it ends a path, so `match` and
  `if` values and functions may leave by it. Under a numeric contract the proof
  must show it is never reached; refused under the memory and record models.
  `Vec.release_empty` and `HashMap.release` on a non-empty value now trap at
  once. `Vec.set` for copied elements writes through `at_mut` in place
  (0.272 s to 0.068 s for 2e7 sets). docs/wasm-unreachable.md.

## `using`

- `using name = value:` discards the owner on every way out of the block,
  including `?`, `return`, `break` and `continue`; an exit that moves it hands
  it on. A rewrite before checking, so nothing new is trusted. The anagrams
  program's `key` lost its three manual closes. `HashMap.get` probes once
  (lookups about 30% faster). docs/wasm-using.md.
- Proof statements are written on several threads, with the same statements in
  the same order: a cached build of `hello` went from 17.6 ms to about 15 ms
  (docs/wasm-compile-cost.md). `ledger.wy` uses `using` in its parser.
- The record model can call functions with numeric contracts
  (docs/wasm-record-calls.md). `String.push_scalar` now keeps its UTF-8
  arithmetic in three proved helpers and has one path to state. Trusted: the
  new rule in the record model.
- A generic record constructor takes its type from its field values when none
  is written (`tests/fixtures/generic/constructed.wy`).
- `using` disposes of its owner once per exit, as the ownership checker sees
  it: nothing where it was handed on, a refusal where only some paths handed it
  on. `programs/diff.wy` compares two texts line by line (longest common
  subsequence) with `using`, `Vec<String>` and a table in a `Vec<U64>`.
- `items[i]` reads a Vec element, trapping past the end, as fast as `get`
  (docs/wasm-index.md); the inliner runs up to three rounds.
- `==` and `!=` call `Eq.eq` on values other than numbers and Bools; the
  library's `Same` is now `Eq` (docs/wasm-traits.md). `items[i] = v` and
  `items[i] += v` store through `index_set` (docs/wasm-index.md).
- Traits with type parameters (`trait Equivalent<K>`); HashMap `get`,
  `get_mut`, `contains` and `remove` take any key that is `Equivalent` to the
  key type, so a Text literal finds a String key (docs/wasm-traits.md).

## 2026-11-17: tasks, and proofs that remove checks

Details and reasons are in docs/decisions.md; the task reference is docs/wasm-tasks.md.

- `ca13bf29` `c5c779db` `849b486a` Tasks: functions that pause run on their own stacks, unwinding at a pause and rewinding at a step; only locals live after a pause are saved. The echo server as one sequential function per connection. Trusted: the unwinding emitter (src/compiler/pause.rs, emit.rs); no proof sees a pause.
- `a33f2a57` `d34250e8` `2fdc322b` `6906d02d` `9794dafb` `join`/`first` over tasks, wake words, cancellation, timers made with the clock, client stepping through a `Net`, a read with a time limit from `first`.
- `2fdc322b` The task echo server even with the hand-written event loop (one busy-machine run): the host answers the `recv` after a short read without the system call, and stops copying.
- `23e5ebfe` `2fed8e88` `7a51c250` A function whose scalar proof covers every `+ - *` of its body is emitted without overflow checks; `nopanic fn` is checked against what is emitted. Trusted: that the walk's record of covered nodes matches what is emitted (node identity; review round 30).
- `1043973e` Contracts on trait methods over numbers and results: implementations prove them, generic callers rely on them.
- Review rounds 26 to 32 (adversarial, by subagents) each found real bugs, all fixed and kept as fixtures; round 31 found none. Notable: a capability leak through forged timers (29), a false `requires` on a task entry wrapping silently once checks were removed (30), lost waits for a parked task (32).
- Open: the Vec proof at 2.5-3 s against a 2 s target (Lean startup and a one-thread helper; the levers change the trusted helper or SDK); `concurrently:`/`first:` blocks; contracts about `self`'s fields.

## 2026-11-17, later: proofs that match the code, borrows, tasks from closures

Following the user's five priorities (proofs describe what runs; libraries say what they borrow;
packages as a trust boundary; everyday async; proof work off the edit path).

- `0a0181ae` `da7c5799` Every proof generator evaluates in the emitter's order: compound stores
  read the destination first, by-value records are copied when their argument is evaluated, a
  record store builds the whole record first. Each mismatch let a proved `+` be emitted
  unchecked and wrap; all have refused fixtures. Still trusted: that generators and emitter agree.
- `e4862cb4` `91715991` `8fdf1a4b` `df2a89fd` `borrows Variant from parameter`: what each variant
  of a result borrows, checked by the borrow checker at each `return`, no proof. Three
  adversarial reviews: two found use-after-free holes (fixed, kept as fixtures), the third none.
- `e629a9af` A config package imported by an application, holding no capability: plain values
  borrowed from the file's text, escaped ones decoded into the caller's scratch and borrowed
  from it. `c1f85c3c` `http.head`/`http.header` read a response in place.
- `7a10c4bb` `wasm-caps --by-file`: per file, what it can use and its unchecked memory operations.
- `8415ca0d` Refused proofs and explanations are cached: the fixture suite went from 637 s to
  104 s, now about 60 s. An application edit that changes no contract already did no Lean work.
- `da6465f5` Closures that take over what they capture run once; `Task.spawn` takes one;
  `race`/`both` handle a task that cannot be made. `412e7ddb` A `pure fn` takes no such work.
- `f608c4de` `00a47708` A `borrows` result bound with `let` is split at its match; a function
  may forward a call's borrows variant by variant.
- `ec822dfe` `b5e6e8b4` `wasm-caps --policy`: each file held to the authority a policy line gives
  it; a file no line covers is a violation.
- `acc6dc88` A download package streams a response into a file under `race`'s time limit; the
  policy holds it to `Socket, File<Write>`. `dc4ebbdc` `TaskSet`: any number of closures as
  tasks, all or nothing. `6e05fccb` The echo task server spawns a closure.
- Reviews of once closures found work copied and run twice (fixed) and `wasm-caps` missing
  closure bodies (fixed).
- 2026-11-18, evening: F64 (docs/wasm-f64.md): n-body and spectral-norm give the reference
  outputs, mandelbrot matches a model; n-body 10^6 steps 0.05 s vs Go 0.03 s; exact decimal
  output. Arrays alone up to 256 numbers. Packages by name (`PROGRAM.packages`),
  `wasm-caps --repin`, the build's policy check on its own checked program.
- 2026-11-18, later: fixed arrays indexed in place live in the frame (fannkuch n=9 85 -> 28 ms,
  the slice version 24 ms); `s[i] = s[j]` accepted; every build applies `PROGRAM.policy`, on the
  bundle it compiles, with paths relative to the policy and `; digest` pins; the host's "try"
  socket operations are non-blocking (a stalled write held every task 2 s on macOS); file
  handles reused, `Dir.close`; `run_task` refuses inside a task; proved writes skip the token
  audit; the download package follows status, `Content-Length` and chunked bodies.
  Awaiting the user: trusted precompiled modules (7 ms of startup), the proof-installation hash.
- HTTP file server package (`tests/fixtures/packages/httpd/`): a task per connection, a time
  limit, a read-only subtree per connection, held by an enforced policy; `send` now flushes;
  `--partial-output` for traps; once closures with arguments (`WitchyOnce<n>`).
- `c22f9759` Trait contracts may read a pure method of the same trait (`requires index <
  self.len()`); implementations of a pure trait method are pure. An implementation's `len`
  must read one field for it to prove such a clause.
- `ebf2ef44` Work cancelled before its task starts runs nothing (only at the task's start).
- Done 2026-11-18: once closures with arguments (`measure(4)`, `closures/once-argument-call.wy`). Open, from then: the UTF-8 validator's functional proof (deferred);
  fresh-proof cost, 4.5 s of 5.6 s being the Lean installation hashed twice (a trust-base
  choice for the user); the Vec proof at 2.2-2.5 s against a 2 s target.

## 2026-11-18, night: benchmarks against Go, and what they found

Eight Benchmarks Game programs run with output matching the reference or a matched model
(`tests/fixtures/benchmarks/`, `tests/fixtures/f64/`), and each is timed against the same
algorithm in Go (`benchmarks/go/`, table in docs/benchmarks.md): fasta 1.19x, mandelbrot
1.23x, sort 1.5x, n-body 1.7x, spectral-norm 1.87x, HashMap 2.0x, binary-trees 2.5x, String
building 5.2x.

- Found and fixed while writing them: the inliner zeroed f64 locals as i64 (invalid Wasm); an
  F64 live across a pause was saved as i32; `write_f64` was wrong in about one case in twenty
  (now exact, half to even); console output was capped at a fixed 64 KiB (`--output-bytes`);
  `HashMap.release` on a full map trapped without saying so at the call.
- Made faster: `sort` (introsort through a held element, 1.8x), `Heap` (1.75x), counting `for`
  loops step without an overflow check, `f64.const` for float literals.
- Measured and not kept: hoisting loop-invariant loads (no gain; a `v[j]` read is 0.36 ns), a
  slot-table HashMap (4%).
- The host names a refused flag and why. F64 may be used inside functions whose contracts are
  about integers.

Waiting on the user, all recorded where they came up:
- docs/proposal-cell-ranges.md: changing a run of cells' state in one allocator call (option
  A), or inline (option B). It is what String, HashMap, Vec growth and binary-trees still pay.
- Trusted precompiled modules (about 7 ms of each launch).
- The Lean installation hash and a multi-threaded proof helper (fresh proofs, and the Vec
  proof's 2.1-2.25 s against the 2 s target).
- Whether the default heap (3 pages, 64 allocations) and host memory grant should be larger:
  binary-trees and the sort benchmark need `--heap-pages`/`--slots`/`--memory-pages`.

### The book's syntax that Witchy 2 does not take (checked 2026-11-18)

About 90 small programs, one or more per construct of the original book's tour chapters and
operator table, found these refused (programs kept in the session's scratch `bookgaps/`):

- Done since: a body ending in its value; `for i in a..=b`; bit operations (docs/wasm-bits.md);
  and on 2026-11-18 the rest listed in the section below.
- Duplicates of an existing spelling, not planned: `Pat -> expr` arms (Witchy 2: `Pat:`), `??`
  (`unwrap_or`), `?` on an Option (`ok_or`), bare `self` (`self: T`).
- Considered 2026-11-18 and not taken for now (docs/decisions.md): row extension, list
  patterns and comprehensions, `e? "context"`, function types in variants, function names as
  values.
- Rarely used, not planned: `@dynamic`, `dyn`, `mode opt`, `region`, `comptime`, tagged strings.

## 2026-11-18, day and evening: the book's syntax, dogfood programs, four reviews

Proved: nothing new; every addition below is a rewrite into constructs already checked, or a
library impl, so the trusted set grew only by those rewrites. The language at a glance:
docs/language.md (its examples run as `programs/tour.wy`).

- Syntax from the book: `if let` (statement and one-line value), `while let`, labelled and
  default arguments, record spread `..base`, match guards, tuples (`let`/`var (a, b): T`),
  `derive(Eq, Hash, Before, Discard)`, `impl Trait` parameters, closures without a result
  type, generator methods (`-> Iter<T>` accepted), duration literals, `\0`/`\$`, a binding
  last arm on numbers, one-line `if` as a statement, lines continuing inside brackets.
- Also: a Vec element's fields by index (`points[i].x`, `cells[0].count += 10`); `Hash`/`Eq`
  for U8, U16, U32, Bool; program types may share names with the library's type parameters.
- The integration suites (`wasm_scalar`, `wasm_cli`) were repaired from 32/78 to 78/78 and
  6/6; five bugs they had hidden were fixed.
- Six dogfood programs (`programs/inventory`, `words`, `calc`, `ledger`, `scheduler`,
  `fs/csv-round-trip`) found the gaps behind several of the above.
- Four adversarial reviews: one soundness hole (`F64.to_i64` in a contracted function could
  trap) and four breaks of written-order evaluation (guards that change the matched value,
  record spread, Vec element field writes, a `?` in an inline `if` moved ahead of a trap), all
  fixed with the reproducers as fixtures.
- Considered and not taken, with reasons in docs/decisions.md: row extension, list patterns
  and comprehensions, `e? "context"`, function types in variants, function names as values,
  a distinct `Duration` type (waits for operator traits).
- Left: an inline `if` that moves a value read earlier in its statement is refused (bind the
  read); a spread base of another record type with the same field names is accepted.

Later the same night:
- Operators on a program's types: `+ - *` through `Add`/`Sub`/`Mul` (`import ops`), `< <= > >=`
  through `Before`, `+=` on records; `Duration` is its own type (`import time`) that duration
  literals make, with `clock.since` and `clock.sleep_for` beside the millisecond methods.
- What precedes a `?` or an inline `if` in a statement is bound first, in written order,
  instead of refusing ("bind that call to a name first" is gone).
- Fewer annotations: generator locals and closure captures take the type their value shows,
  including a call of a plain function whose signature names it (scanned before parsing).
- Three more review rounds of these (the statement binding twice): defects found in each were
  fixed with their reproducers as fixtures; one long-standing panic (a callee computed by an
  inline `if`) became a refusal.

- `Duration` shows itself (`"${d}"` is `1m30s`) and scales (`250ms * 4`, a `Scale` trait).
- Messages: nine refusals a newcomer meets now say what to do (`var` for a changed `let`, the
  type's largest value, how many arguments, that a capability is a parameter, the expected
  token, ...: `tests/fixtures/messages/`).
- A run that traps says where and why: the compiler writes a Wasm `name` section and a code
  before each check's `unreachable`, and the host prints ``the run stopped in `main`: an
  index out of range`` (no measurable run-time cost on the benchmarks). The host admits that
  one section under a strict check; its review found and fixed an index bound and names that
  could write to stderr.
- Commits now go through a gate that runs every suite first.

- Loops and text: `for (a, b) in`, `for (i, x) in v.enumerate()`, `String.chars()` and
  literal `chars()`, `for b in span`, every integer in `${...}`; generators `return` at any
  depth and filter containers (`for x in v: if keep(*x): yield *x`).

### Cell ranges (docs/proposal-cell-ranges.md), the same day

Measured first: `push_text` spends 24 of 187 ms reading the literal; the rest is one allocator
call per byte. The allocator's `cell` helper gained range modes (check a whole run, then mark
it once). `memory.move_prefix_unchecked` is proved and makes Vec growth two helper calls
(a million U64 pushes 16.4 to 14.0 ms, 200000 Strings 11.2 to 8.9 ms).
`memory.copy_into_unchecked` is proved for elements without owners; memory contracts take two
borrowed Vecs (one written). With it `push_string` measured 166.7 to 117.7 ms, but the method
added 370 ms to the Vec library's proof (2.48 s against a ~2 s budget), so it is held back as a
run fixture until its case splits are prelude lemmas, which needs a new trusted helper and SDK
generation (an operator's step). A review of the range work found an older hole it made easy
to reach (a call forgot only one of two written buffers, so a String could be released twice);
fixed with its reproducers as fixtures. `str` is unchanged: its literal is not a buffer.

Fixtures 954, unit tests 265, host tests 12, integration 78 + 6, all passing; Vec proof 2.07 s.

## 2026-11-18, night: brands (first prototype)

Following `/Users/cobrien/workspace/generic/witchy-native-brands-20261118/report.md`, gate 1 and
the erasure part of gate 2 (docs/design-brands.md, `tests/fixtures/brands/`, 24 programs):

- `brand roads.nodes() as nodes:` opens a graph; the opened call's `Fresh` type parameter gets a
  brand new at that opening. Helpers say `G: Brand`; no call writes a brand.
- A row of another opening is refused at the argument, naming both origins
  ("this `Node` comes from `rail`; what it is used with here was opened from `roads`"), even when
  both graphs have the same count. Refused too: two openings of one graph, recursion through one
  opening, a second issue under a brand in use, an opening nested in another's value, escape
  from the block, a call made without `brand`, a non-brand given as one, changing the graph while
  it is open.
- `import graph`: `Graph`, `Nodes<G>`, `Node<G>`, `Neighbors<G>`, `Distances<G>`. The proposal's
  first step of a shortest-path search runs as written; Dijkstra with shared row types gives the
  same output at the same speed.
- Brands are erased when instances are made, at any depth: eight openings went from 142
  functions to 65.
- Adversarial review: no way found to break the guarantee in about 75 programs; three
  usability defects fixed.

Not yet: the shared-cell permit (GhostCell; needs one reference to hold two loans), a brand per
run rather than per place in the source (needed only if rows may ever leave their block), and
removing the tables' bounds checks by proof.

A dogfood program, `programs/word-graph.wy`, counts the words of a text by Span with the map's
own `get`/`get_mut`, links each word to the next in a graph, and walks it breadth-first over
the graph's rows. It found three gaps, now closed: no way to walk a Span's words
(`span.words()`, `views/words.wy`), `Graph` not `Discard` (so not usable with `using`), and an
unhelpful message for `&v[i]` of a Vec (`messages/vec-index-place.refused.wy`).

A second dogfood program, `programs/inventory.wy` (lines parsed into records, totals by category
in a map looked up by Span, sorted with `Before`, printed by slot), found: a missing trait that
`derive` gives was not named as derivable (now said, `messages/derive-hint.refused.wy`); and a
gap left open, that returning a word from inside `for word in span.words()` is refused, since the
checker reads the word as borrowed from the loop's own `Words` value rather than from what that
value points into (a `split` does the same job). Closed the same night: `&v` of a view takes a loan on its storage
(`VIEW_ITSELF`) apart from what it carries; when no reference in a call's result can reach that
storage by type, that loan is released and the inherited ones stay
(`views/word-returned.wy`, and `programs/inventory.wy` now finds its category that way; refused still: the String closed meanwhile, and a slice of a view's
own inline array, `views/view-storage-returned.refused.wy`).

A third dogfood program, `programs/maze.wy` (breadth-first search: a `Deque` queue, a closure
over the maze's bytes for "open", `Option` neighbour steps, `?` under nested `using`), found no
defect. Two things a newcomer meets: an owner made before a later `?` must be under `using` (the
refusal says so, rightly: `?` would leak it), and a variable a closure captures needs its type
written (`let bytes: &[U8] = text.as_bytes()`), since closures are lowered in the parser before
types are known. The second is a gap, left open.

## 2026-11-18, late: optimisations from guarantees, proofs that say more

Faster:
- `5eea20dd` The library sorts a slice: the sort benchmark from 1.40x Go to 1.06-1.10x.
- `deaf5879` The host compiles a module's functions in parallel: startup about halved
  (nbody 7.8 to 3.4 ms); spectral 1.11x, nbody 1.33x, fasta 1.24x Go.
- `e7762377` `[v; n]` into a framed array is a loop: smaller modules, less to compile.
- `0a04b5aa`, `aa795209` A `while` counter bounded by its condition is incremented unchecked.

Proofs:
- `9aac5cfb` and three review rounds: type invariants over plain fields, assumed of every value
  received and shown wherever one is made or changed; `deposit` loses both its checks.
- `6417c7ac`, `17e60828`, `90545834` Contracts name what a returned variant holds; callers
  read it in the matching arm; a Result handed on carries it; validating constructors
  (`return Ok(Account(...))`) of invariant types.

Language: `&v[i]` lends a container's element; `&array` is its slice; literal arithmetic in
slice stores; a `?` leaving an owner behind is named at the `?`; "did you mean" for methods,
functions, fields, variables and types; once-closures with arguments; capture types from
method results; a branch that leaves the function ends the loans it does not read.

Measured, not taken: HashMap per-borrow validation (1-3%); the range modes' token audit
(4.5% on str, a deliberate defence); `String.byte_or_end` through a slice (no change).

Open: startup is now compilation, which caching compiled code would remove (a trust choice
for the user, parked); map and trees are allocator-bound.

## 2026-11-18, night

Faster:
- `565f98bc` Range marks take a bitmap word at a time at any offset: str 76 to 64 ms (1.58x Go).
- `77c905da`, `1f188f3e` `for` over a shared slice, or a type's `elements`/`elements_mut` (Vec),
  walks without a per-element test or `Result`: 0.99 to 0.51 ns an element, `&mut` likewise.
- `ce45e6db`, `c1466936`, `e06d7c8d` A `for` over a generator runs the generator's body in
  place: counting generator 27 to 4.1 ms, `String.chars` 71 to 46 ms, `Text.chars` 48 to 28 ms.
- `4e38dfc9` `scalar_at` returns ASCII at once; `d4d94790` `utf8_spare` inlines (push -4%).
- New benchmark `words` (word frequencies, 2.47x Go).

Correctness (from reviews 26-29 and the Astra audit): storage proofs follow `let` aliases;
proof facts carry through bindings; `&shelf[0]` needs a real element borrow; interpolation
names the library's traits; the trap report follows proved reads; generators refuse built-in
names, check scope and `let` before flattening; in-place generators resolve as calls do.

Language: sum and generic types `derive`; `&s[i]`/`&mut s[i]` on slices; `*(&mut place) = x`;
generators as locals of generators; a `while` bound written `limit > i`; clearer refusals for
dereferences, static and value method misses (`write_int` finds `write_u64`).

Measured, not taken: checked `x * k` by comparison; an ASCII fast path inside `chars` (slower
while stepped); slice compare in `span_is`; index loops (a bounds test costs nothing measurable).

Parked for the user: the cell helper's per-byte call (a third of `words`) could be emitted in
the caller only by letting program code write allocator metadata, a trust-boundary choice.

Correction, same night: generic records already hold owners (`Pair<String>` is built, opened
and closed); what refused it was the derived impls, whose `T` lacked `Any`. Derived impls now bound
`T: Any + Trait` (`derive/generic-owner.wy`). Nothing more to build there.

## 2026-11-18, late night: reads with no tests, prover reach, messages

Gate at 1301 fixtures, 268 unit tests. Each item has a decisions.md entry and fixtures.

- **Reads with no tests, by compiler reasoning** (no Lean; reviews 32 and 33 found nothing):
  `for j in a..s.len()` over a shared slice of Copy elements (`iteration::COUNTED`); the same
  over a shared `&Vec` outside contracted functions (through `v.elements()` bound first);
  `i < s.len() && ... s[i]` (`check.rs` `guarded_reads`). spectral-norm now beats Go (0.89x).
- **Prover:** product bounds through differences, quotients and products; `part * L / whole <= L`;
  quotient and remainder by a variable (`Nat.div_le_self`, `Nat.mod_lt`, `Nat.mod_le`); bounds
  read out of `&&` conjunctions; `xs.len()` of a slice in a contract. Review 31 found nothing.
- **Language:** `let x = *call()`; `==` with bare variants either side, generic sums included, in
  written order (`Type.eq(&a, &b)`); generic types holding owners derive; variants and functions
  named like the library's type parameters (`E`, `T`, `K`) no longer break it; `Discard for F64`.
- **Messages:** one build reports a checking refusal in each function that has one; a type
  mismatch names both types; an operator on a Result says to take the value out; a missing trait
  on a built-in type names the import; `&name` of a reference; interpolating a reference.
- **Measured, not changed:** dropping first zero stores (no fuel saved); `match` jump tables; compile
  time (about 15 ms a program in release); `span_is` speed. `words`, `str`, `map`, `trees` wait on
  the allocator-helper decision parked for the user.

## 2026-11-19, overnight: Astra's HashMap findings, text speed, host randomness, checks against references

Taken from Astra's two hashbrown passes: `take_any` closes its hole (it left `get` missing a live key);
removal shifts back only entries whose probe path crosses the hole (2.2x on integer churn, 3.6x with
String keys); `for_entries(n)` sizes by entries; growth places moved entries without comparing keys;
`hash_bytes`, proved, one remainder per six bytes (no overflow check emitted; same hashes). Then the
text library read through byte slices instead of `byte_at`'s Result: `String.eq`, `compare`, `chars`,
`scalar_at`, `next`, `find_byte`, `parse_u64`, `write_string`, and `push_slice` copies bytes
(`String.push_part`). 127-byte String-key lookups 205.5 -> 110.6 ms; String sort -13%; CSV split -18%.
The host's randomness was opened per draw (~40 µs); now read in blocks (700x).

Random operations checked against simple references, all correct and now fixtures: HashMap (churn,
walk, take_any, String keys), Heap, Deque, Vec mutators, sort (U64 and String), UTF-8 round trip,
String.cut, push_slice ends, words(), parse_u64, JSON string escapes, task scheduling. Programs: JSON
order, wc, word-frequency top five, maze BFS, card shuffle. Review 35 (borrow rewrites): no
unsoundness; one debug-only panic fixed.

Messages: lifetimes, dashed imports, unknown bounds, shadowed locals, number operators on references,
`match` on a non-sum, copies of owners, a missing method (type, method, field: derive errors).
Language: `String == Text`, `&v[i]` arguments to generic methods, reference parameters beside an `if`
value, literals inside `${}` (they read another literal), format's `Show` is `Render`.

Open, for the author: Option B (the cell helper; words/trees/str are bound by it), a probe-once
insert-or-update HashMap API (`map` probes twice per miss), `static for` / derive laws (Astra is
revising the handoff toward derives whose Eq/Hash agreement is proved).

### 2026-11-19, later (62 commits after 87e8bff6; gate 1409 fixtures, 272 unit tests)

Faster, each measured with a release build per variant (docs/decisions.md, benchmarks.md):
- Lookups of String keys by long Text literals 142 -> 69 ms (-52%): Text's hash reduces twice per
  word (proved), and `Text.equivalent` compares a word at a time through the new `String.word_at`.
  By long Strings 83 -> 64 ms (`String == String` a word at a time from eight bytes). Short keys
  unchanged.
- Numbers into a String (`push_u64`, `${n}`, `Render`) a word at a time, `String.push_decimal`:
  -31% on a formatting run.
- Three proofs that only removed checks (fasta's generator, `HashMap.get`) saved fuel, not time;
  the ones that pay let the code do less work. docs/performance-idioms.md says so.
- Tonight's compiler against 4862ebf7: modules 0-2.2% larger, startup, run and compile time
  unchanged.

Proof failures name what failed: the kind of check (`a \`-\` that may go below zero`), the local it
is assigned to or the returned value, and for a call the callee's `requires` clause not shown.
Contracts over large literals prove (reducible `assumption` first). A trap in an inlined function
names the function; the host says when a trapped run's output was withheld.

Language: typed `let Ok(x): &T = ... else` with reference payloads; plain quotes inside `${...}`
(and a `}` inside them); numbers and module constants inside `${...}` (they were refused);
`Console.write_format`; `Text == Text`, `Text` is `Discard`. Many refusals reworded, several found
by the fuzzer's new ranking of refusal messages (`scripts/fuzz-mutants.py`, which also mutates
quotes and braces now). Fuzz: 5,000 + 3,000 mutants, no crash.

A near miss, never committed: letting a blanket impl stand in for `Equivalent<Text>` reached
Text's own `Equivalent<String>` body with a Text (methods are found by type and name). The refusal
stays, now saying which impl the type has; six routes to it are refusal fixtures. For the derive
work: a type has one impl of a trait whatever its arguments.

Recorded for the author (TCB): a failed allocation scans the heap (~55 µs each; a free count would
fail fast); a copyable map's teardown is a helper call per cell (a clear-range mode would make it
one). Baselines for the derive goal's three examples are in benchmarks.md.

Open, for the author: as above, plus those two allocator items; `trees` would want a single-cell
owner type (a node is a Vec of two).

#### Later still (2026-11-19, after facfe652; gate 1422 fixtures)

- Console output a word at a time: a third output import `print_word(handle, word)` (host and
  profile changed, reviewed independently: no defect); `write`, `write_string`, `write_u64` and a
  float's digits use it (3.5x on a write-heavy run); `WordWriter` for byte-at-a-time output
  (fasta -10%). Host and compiler go together: an older host refuses the new modules.
- `read_line` appends ASCII runs a word at a time (-30%); `String.push_ascii`.
- `Span.to_string` copies eight bytes to a write (-48%); one-digit `push_decimal` back through
  `push` (a one-byte word write is dearer than `push`'s: JSON punctuation through it was +15%).
- New text operations: `find_text` (String and Span), `Span.lines()`, `starts_with_text`,
  `ends_with_text`, `contains_text`, `parse_i64` on Spans. Programs: grep, config reader.
- Messages: function called as a method, union tags need their dot, input grant names its flag,
  a contract's missing `ensures`.
- Not taken, recorded: an ASCII fast path in `utf8_valid` (the memory model refuses bit
  operations, `continue`, `&&` guards, Bool stores and calls to scalar-proved helpers).

Open, for the author, added: library imports carry their type names to the importer's importers
(an internal import made `Words` visible to programs that import `int`, breaking a variant named
`Words`); the variant-name rule, or what an import passes on, is a language decision.

#### Later again (2026-11-19, after d94d615d; gate 1435 fixtures, 273 unit tests)

- F64 text both ways, bit-exact with Python: `parse_f64` (a U64 front for short numbers; an
  exact reader with algorithm D over 32-bit limbs for the rest; 9,852 + 200,015 strings
  compared) and `push_f64` / `${x}` (shortest round trip in `repr` form; F64 arithmetic proves
  most values' digits with one exact division, Burger-Dybvig for the rest; 102,128 compared).
  `Json.number()` reads a JSON number as an F64.
- `sort` orders F64 by IEEE totalOrder (NaNs last: `F64.bits` makes every NaN alike, so the
  order is the same on every machine). `min`/`max`/`clamp` on U64 and I64; IEEE `min`/`max` on F64.
- Messages: a lexing error inside `${...}` is placed at the literal (it named a library file);
  an exponent with no point; a method on a decimal literal; alphabetical record label order.
  Type hints for `F64.to_i64(x)` and `F64.bits(x)` results.
- Programs: price-stats (CSV floats), float-calculator (recursive descent).
- Checked and not taken: `[U8].word_at` (a node through checker, emitter and both proof
  generators for ~5% of `words`). All four slowest benchmarks (`words`, `trees`, `map`, `str`)
  wait on parked decisions.

Open, for the author, added: whether `1e300` (no point) should be an F64 literal; it is refused,
now with a message saying `1.0e300`.

#### Last of 2026-11-19 (after f2b16ab9; gate 1457 fixtures, 274 unit tests)

- Library: padding (`pad_to`, `push_repeated`, `write_repeated`, `digit_count`), `HashMap.take_next`
  (emptying a map: 1,472 ms -> 1.2 ms for 20,000), Vec `remove_at`/`insert_at`/`reverse`,
  `search`/`index_of`, `import encoding` (base64, hex; 657 inputs against Python), ASCII case,
  `Json.field`/`item`/`json_f64`.
- References generated from source: `docs/library-text.md`, `docs/library-collections.md`
  (`scripts/library-reference.py`); 55 public functions given descriptions.
- Compiler: whole-array copies in the frame are a loop (a `[U64; 128]` copy 3.8 KB -> 0.3 KB);
  the f64 printer 18.8 KB -> 3.9 KB.
- Messages, from ranking 8,000 fuzzed mutants (no crash): indentation (what the line has, what
  fits), a block with no body, two values side by side, a statement at the top level, an unknown
  standard type/function/method names its `import`, an owner indexed by `v[i]`, an untyped
  argument's own error before generic ambiguity, a reference or a view as a stored type argument,
  an anonymous record and traits.
- Programs: word-frequency-table, shortest-paths (Heap), maze-bfs (Deque).
- Tooling: `run-fixtures.py --jobs N`; the gate runs 8 at once (93 s -> 19 s).

Open, for the author, added: wrapping arithmetic (`U64.wrapping_mul`); anonymous records of plain
values satisfying `Discard`; the host's 64 KiB default module limit (JSON plus float printing
passes it at 71 KB).

#### After 1cef8fbb (2026-11-19, late; gate 1464 fixtures, 275 unit tests)

- Input: `read_run`, a second input import (up to seven bytes a call, ending after a newline or
  a byte of 128+); `read_line` on runs: -10% on short lines. Host and compiler change together;
  modules built before it still run.
- JSON: strings in one allocation (`String.with_capacity`), slice-once scanning, error codes on
  `json_tree`; a 968 KB parse 28.4 -> 22.1 ms. The 64-slot default fails a JSON document of five
  objects (code 5): documented; the default itself is the parked heap-size decision.
- Inliner: callees of 32 bytes or less written in past the 40 KB module limit (-3% on large
  programs); 96 bytes was tried and reverted (it pushed `download-app` past the host's 64 KiB).
- Messages: a second place (`⟪at N⟫` written out as `file:line`) for duplicate declarations,
  shadowed names and uses after a move; an unknown standard type, function or method names its
  `import`; missing `main` (misspelt, or a module compiled alone); the commonest refusals of
  8,000 fuzzed mutants (indentation, end of line, a statement at the top level).
- Benchmarks, end of day against Go (all outputs equal): fasta 0.97x, spectral 0.86, sort 1.01,
  mandel 1.19, nbody 1.35, str 1.38, map 1.58, words 1.89, trees 1.95.
- Tooling: the gate builds the development compiler first, runs fixtures 8 at a time and checks
  the generated library references (`scripts/library-reference.py --check`).

#### After e428110b (2026-11-19, evening; gate 1474 fixtures, 275 unit tests)

- Reading lines: `read_run` (seven bytes a host call), plain bytes appended a run at a time,
  `String.clear` in one proved range step (`Vec.clear_mut`). 180,000 log lines 66 -> 57.7 ms;
  200,000 CSV lines 28.8 -> 20.8 ms. Two independent reviews of `read_line` each found a real
  defect (bytes lost after NoRoom; NoRoom for a line that fit); both fixed with fixtures that
  fail on the defective versions. Review library changes of this kind before calling them done.
- Library: `String.replace_text`, `join_texts`, `Vec.extend_copy_mut`, `Vec.clear_mut`,
  `String.reserve`, `String.push` writes a multi-byte scalar in one word, `parse_f64` reads each
  byte once (-18% on short numbers). The library reference now lists functions with nested type
  parameters (`HashMap.get`, `get_mut`, `contains`, `remove` were missing) and shows them.
- JSON: arrays and objects start with room for four; with the above, a 968 KB parse
  28.4 -> 19.9 ms over the day.
- Messages: a Result used as a Bool; the arms a match is missing, written out; the call that
  ends an owner left in scope (`close`, or `discard` for a Vec of owners).
- Programs written as a user would (`programs/csv-summary`, `programs/log-summary`), both equal
  to Python. Each needs 10-60 times the default 1M instruction budget: fuel counts each call of
  the allocator's access helper in full (a data point for the parked budget decision).
- Measured and not kept: inliner threshold 96 (module growth broke a 64 KiB package), body-less
  contract copies and type-walk fast paths (compile time: allocation is spread thin),
  `scalar_at` unrolled, `mandel`'s loop exit (engine-bound: Go fuses multiply-add).
- Benchmark ratios against Go move with Go's own time (words 1.89x-2.50x for identical Witchy
  code): compare Witchy's run time across days (docs/benchmarks.md).
- New decision for the user: memory contracts that model allocation would let `Vec.reserve_mut`
  grow without a placeholder Vec (an allocation, a release and a slot per growth).

#### After 08504286 (2026-11-19, night; gate 1483 fixtures, 278 unit tests)

- Code-size passes over finished bodies (src/compiler/inline.rs `tee`, emit.rs): a constant
  multiplier's check is one comparison; the inliner zeroes fewer locals and lets a final return
  fall off; no zero-offset adds; `local.set x; local.get x` is `local.tee x`; writes to never-read
  locals are dropped (never just before an `unreachable`: it would fake a trap reason); no empty
  `else`. Modules 4.5-5% smaller, run time unchanged, fuel lower; `download-app` 64,645 -> about
  63,800 bytes under the 64 KiB limit. Independently reviewed (one trap-reason defect, fixed).
- `parse_f64` proved, so its fast path has no overflow checks (-21% fuel; time unchanged: the
  general multiply check's 32-bit pretest already skipped its division). Proofs as an
  optimization pay in fuel, not in time, for checks of that kind.
- Programs written as a user would: csv-summary, log-summary, word-wrap, config-fields (JSON
  `text`, `boolean`, `len` added), job-schedule, number-stats; all equal to Python.
- Later the same night: the inliner zeroes a callee's local only when a read can come before its
  write (a write counts when every read lies inside the block that holds it), and calls of one
  callee share its locals. Modules a further 2-7% smaller (sort 17,666 -> 15,992 bytes; locals
  987 -> 695), run time and startup unchanged. Both independently reviewed (no soundness defect;
  a locals-budget cost and an f32/v128 zeroing gap fixed) and checked with
  `scripts/diff-compilers.py` (scalar, signed and `vec` modes; thousands of random programs
  against the compilers before them and the start-of-day compiler, no difference).
- End of the night (55bc8fd8; gate 1483 fixtures, 280 unit tests): locals declared once a type
  and unused ones dropped (modules a further 7-12% smaller); the inliner writes in callees up to
  96 bytes while the program's own code is under 44,000 bytes (JSON 21.4 -> 19.8 ms, packages
  unchanged). Over the day: words 27,726 -> 22,964 bytes, nbody about 16,000 -> 12,426; the
  968 KB JSON parse 28.4 -> 19.8 ms. Every pass checked by `scripts/diff-compilers.py`
  (thousands of random programs, scalar, signed and heap), reviewed independently, and fuzzed
  (6,000 mutants, no crash). `scripts/run-fixtures.py` now reports each run's time.
- Derive-law milestone (2026-11-19, Astra's design; docs/plan-derive-laws.md, all four slices):
  library recipes (`pub derive Key:` with `Self`, `static for f in fields:`, `by:`, `for:`),
  expanded in the parser (src/parser/recipe.rs); `Policy` (Reference, Constant, Fast),
  `HashState` and `HashKey` in library/wasm-hash.wy; sealed `KeyMap` in wasm-hashmap.wy;
  the key law checked from the shape of `Eq`/`HashKey`/`Equivalent` bodies with no proof per
  type (src/compiler/key_law.rs), trust tied to the library's exact files by the build gate
  (src/compiler/files.rs). A derived key compiles byte-identical to the same impls written by
  hand; KeyMap with Fast is 5.1 ms against the legacy HashMap's 4.4 on 60,000 operations.
  Ten adversarial rounds found 17 holes (3,3,2,2,1,2,1,1,1,1), each fixed with a
  `policy/law-*.refused.wy` fixture; an eleventh is running. Open: sums in recipes and the law,
  hygienic recipe names, text literals in recipes, `impl Eq for Span` unread (unreachable
  while maps refuse reference keys). The bulk copy is restored (Astra's proof-budget review);
  the 2 s fresh bare-Vec proof condition is the user's call (2,675 ms without, 3,042 with).
- Afternoon (2026-11-19, through 688b86ba; gate 1541 fixtures, 283 unit, 78 integration):
  the key law converged (sixteen adversarial rounds, holes 3,3,2,2,1,2,1,1,1,1,1,1,1,1,0,0;
  classes closed by refusal: aliases in key types, non-library `eq`/`feed`/`equivalent`,
  blanket key impls, program impls for trusted types, stand-in library files by identity, and
  the gate judging the compiled text, not a re-read of the disk). Recipes gained `for:` (a
  checked `Query` recipe, also for one instance of a generic key) and text literals and
  interpolations (sharing the program's literal table). KeyMap moved to `import keymap` (HashMap
  users stopped paying 17 KB of parse and check: words' warm build 33.5 -> 31.5 ms; the rest of
  the day's growth is library features). The bulk copy is restored per Astra's review; the
  proof-budget gate reports growth as a review note and fails only when it cannot measure.
  Benchmarks unchanged within noise; the allocator's helpers are 14-44% of the programs
  furthest from Go (words, str, map, trees): the parked Option B. Waiting on the user: the 2 s
  bare-Vec fresh-proof condition (2,675 ms without the copy, 3,042 with), Option B, and recipe
  syntax for sums (proposal in docs/plan-derive-laws.md).
- Evening (2026-11-19, through cf307194; gate 1567 fixtures, 286 unit, 78 integration):
  library speedups: one-probe `get_mut` (HashMap, KeyMap, span helpers), ASCII-first
  `String.push`, division-free probe steps: words 70.0 -> 60.8 ms, map 72.8 -> 65.5 (suite vs
  Go: words 1.86x, map 1.51x, trees 1.91x, str 1.43x, sort 0.99x). Proof cache answers refusal
  and acceptance from one key. Ten adversarial rounds on derive recipes used across files closed
  substitution and resolution holes (3, 5, 2, 2, 2, 2, 1, 0, 0) by a fail-closed rule over the
  expansion's bare names (parser.rs `foreign_name`; the structural fix, resolving a recipe's names
  in its own file, is written up in docs/plan-derive-laws.md for the user), and two older holes in
  running generators in place from another file (free names and module qualifiers now must mean
  the same in both files, else the generator steps). Fuzz 6,000: no crash; 3,000 random programs
  agree with the afternoon's compiler. Waiting on the user: recipe name resolution (rule vs
  structural fix), `Self` in impl blocks, the 2 s bare-Vec proof condition, Option B,
  insert-or-update for HashMap, recipe syntax for sums.
- Late evening (2026-11-19, through ffd7ea93+): diff-compilers gained eight self-checking modes
  (maps, keymaps, vecs, strings, derived, containers, generators, crossgen; each program counts
  its own mismatches against a shadow or a second reading, and the tool fails on any): 10,000+
  programs clean; `crossgen` finds the evening's two generator-inlining holes in the compiler from
  before them (161 of 200). Hash spread measured: Fast uniform; the legacy U64 hash crowded
  strided keys into one cell in eight because every grown capacity was a multiple of eight, so
  HashMap grows to odd capacities (`2c + 9`; `for_entries` odd too): strided keys 165 -> 102 ms,
  the suite unchanged. String and derived pair keys spread acceptably; sort is never quadratic
  (organ pipe 1.45x random). The 20,000-mutant fuzz found no crash.
