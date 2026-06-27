---
name: learn-witchy
description: Learn or refresh witchy's syntax, semantics, and capability model before reading or writing witchy code. Use when you need to understand how to write a `.witchy` program, what the language constructs mean, how the capability system works, or what the stdlib offers — by reading the project's canonical, test-covered sources in the right order.
---

# learn-witchy

witchy is a capability-secure language with twin backends: a tree-walking
interpreter (the reference semantics) and a compiled-WASM path that must match
it. **The prime directive of the whole project is parity** — any observable
behavior works (or loudly errors) identically on both backends. Keep this in
mind as you learn: there is one meaning, enforced by differential tests.

Your goal here is to become fluent enough to read and write correct witchy.
Do that by reading the sources below — **not by guessing from memory.** witchy's
surface (off-side layout, Rust-flavored words, capability-passing) is distinct
enough that recalled "looks like Python/Rust" intuitions will mislead you.

## Source-of-truth priority

When sources disagree, trust them in this order. Earlier wins.

1. **`src/` + `std/*.witchy` + the tests** — what actually IS. `src/interpreter.rs`
   is the reference; `src/example_tests.rs` holds the differential tests.
2. **`spec/language.md`** — the authoritative syntax + semantics reference. Every
   ` ```witchy ` block in it is a complete program the suite type-checks and runs.
3. **`spec/capabilities.md`**, **`spec/stdlib.md`** — the security model and the
   module-by-module API. `stdlib.md` is **generated** from `std/*.witchy`
   doc-comments — read it, never hand-edit it.
4. **`book/src/`** — the narrative tour (teaching order, prose, worked project).
   Friendly, but defer to `spec/` and code for exact behavior.

Prose can go stale (the build executes ` ```witchy ` code blocks but cannot
catch a wrong *prose* claim). When in doubt, prefer a runnable example or a test
over a sentence.

## Reading path

**Fast path — productive in three reads.** Do this first, in order:

1. `spec/language.md` — read it through. It is ~800 lines and is the single best
   artifact: lexical structure, types, functions, `match`, generics/traits,
   errors, and how capabilities thread through `main`. The examples are real.
2. `examples/README.md` — the index. It maps every concept to a runnable example
   (start-here table, the capability system, concurrency, multi-package projects).
3. Read 4–6 example sources end to end, e.g.
   `examples/hello/src/hello.witchy` (functions, `match`, the `Console` cap),
   `examples/records/`, `examples/result/` + `examples/try/` (`Option`/`Result`,
   `?`), `examples/traits/` + `examples/generics/`, and one capability example
   (`examples/capability_rights/` or `examples/files/`).

**Thorough path — add as needed:**

- `spec/capabilities.md`, then the `book/src/capabilities-*.md` chapters. The
  capability system is the heart of the language; understand authority-as-a-value,
  narrowing/attenuation (`cap as Type`), and the sandbox.
- `spec/stdlib.md` for the API surface (`list`, `string`, `json`, `iter`,
  `http`/`server`, `task`/`chan`, `time`, …).
- `book/src/SUMMARY.md` is the full tour outline — follow it for a guided
  sequence (values → functions → data → errors → generics → iterators →
  comptime → capabilities → concurrency → packages → backends → testing).
- Concurrency: `spec/language.md`'s async section + `examples/channels/`,
  `examples/async_tasks/`, `examples/worker_pool/`. The model is Go/CSP
  (`async`/`await`, `spawn`, first-class channels) on a pure-witchy executor.

## Verify by running, not by assuming

The `witchy` binary on PATH runs programs. Read code, then confirm behavior:

```sh
witchy examples/hello/src/hello.witchy          # run it
witchy check   examples/hello/src/hello.witchy  # type-check (capabilities included)
witchy caps    examples/hello/src/hello.witchy  # show its capability footprint
witchy sandbox examples/hello/src/hello.witchy  # run confined in the WASM VM
witchy test    examples/records                 # run a rune's tests
witchy fmt     <file>                            # canonical formatting (4-space layout)
```

A good self-check that you've actually learned it: write a tiny program in
`scratch/` and get it through `witchy check` then `witchy` (run), reading the
type errors — witchy's errors are written to teach the correct form.

## Things that will bite you

- **Layout is significant** (off-side rule): blocks open with a trailing `:` and
  are indentation-delimited, 4 spaces per level. Not braces.
- **Capabilities are explicit values**, threaded from `main(console: Console, …)`
  inward. A function that does I/O takes the capability as a parameter; there are
  no ambient globals. This is the part most unlike other languages — study it.
- **Two backends, one meaning.** If you ever find behavior that differs between
  `witchy <file>` and `witchy sandbox <file>`, that is a bug, not a feature.
- If you have edited `src/` (the compiler) yourself, the PATH `witchy` is the
  **release** binary and may be stale; build and use `./target/debug/witchy`
  or `cargo build --release`. For pure language learning (running existing
  examples), the PATH binary is fine.

When you finish, you should be able to read any file under `examples/` without
surprise and write a small capability-using program from scratch.
