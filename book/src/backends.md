# Under the Hood: Two Backends, One Meaning

witchy runs two ways, and understanding why — and what holds them together —
explains a lot of the language's design choices.

| Backend | How | Role |
|---|---|---|
| **Compiled WebAssembly** | lowered to a structured IR, encoded to a wasm binary, run on wasmtime | the run path: every program runs here — confinement *and* speed, and the browser playground |
| **Interpreter** | tree-walking, in Rust | the *reference* semantics: the independent oracle every compiled run is checked against |

```sh
witchy program.witchy            # compile to wasm, run with a development grant
witchy sandbox program.witchy    # compile to wasm, granted exactly its footprint
```

Both commands run the **compiled** backend; they differ only in the capability
grant. The interpreter isn't a way to run your program — it's the reference
implementation that *defines* what your program means, and the yardstick the
compiled backend is held to.

The split is deliberate: the interpreter optimizes for being *obviously
correct* (it defines the semantics), the WASM tier for being *fast and
confined* (the capability boundary is the VM boundary — an ungranted host
function isn't denied, it simply does not exist in the instance). Two is
also the right number: every backend added is another implementation of the
semantics that parity has to hold to zero divergence, so witchy spends that
budget on exactly one fast tier.

## Parity: the invariant that makes it safe

These aren't two languages that happen to look alike. They are two
implementations of *one* language, held to a single rule:

> A program produces **identical** output on every backend — including identical
> failure behavior — or it doesn't compile.

The interpreter is the reference: it defines what a program *means*. The
compiled tier must match it. The project's test suite contains hundreds of
*differential* tests that run a program on the interpreter and the compiled
backend and assert the outputs are equal, plus a property-based fuzzer
generating programs to try to pry them apart, plus a CI sweep that runs
every example through `witchy parity` — the project's own
verify-the-compiler harness. (It ships in the CLI so the claim is
inspectable, not because your workflow needs it: parity checks **witchy**,
not your program.)

Parity covers the boring-but-critical edges, not just the happy path:

- `Int` arithmetic **wraps** on overflow everywhere (that's why it wraps rather
  than being undefined — a portable choice both backends can honor).
- Float formatting is the shortest round-trip on every backend; `NaN` and
  infinities behave identically; ordering a `NaN` errors on both.
- Structural equality is deep and identical for every comparable type.
- Out-of-bounds indexing, divide-by-zero, parse overflow, and `fail` abort on
  *every* backend — a trap in the VM, a runtime error in the interpreter, never
  a silently different result.

When a backend genuinely can't express something the same way — historically,
say, comparing certain generic types — witchy makes it a **loud compile-time
error** rather than letting the two backends diverge. "Zero silent divergence"
is the design's north star.

## Why this matters to you

This is the property that makes the sandbox trustworthy, and it's worth saying
plainly: **the program you run is the program you deploy** — `witchy run` and
`witchy sandbox` are the same compiled backend, differing only in how much
authority the host hands over. Parity is what lets that single run path be
trusted: the compiled backend is mechanically proven, program by program, to
match an independent reference implementation. Without it, "it worked when I
tested it" and "it does the right thing in the sandbox" would lean on a fast
tier no one had checked. With it, they're one fact, mechanically enforced.

And it asks nothing of you. There is no "portable subset" to stay inside —
the portable language is simply the language, covering the whole standard
library (the native intrinsics, networking) and the full concurrency model
(`async`/`await`, `spawn`, and channels), which runs on a cooperative executor
written in pure witchy — so a concurrent run is byte-identical on both
backends. On the rare edge a backend genuinely can't express the same way —
historically, comparing certain generic types — the compiler stops you with a
loud error at build time. You never run a verification step; you never guess.

## A note on memory

The compiled tier has no garbage collector — and doesn't miss it. Memory is a
bump arena reclaimed at *structural* lifetimes: the program's exit,
compiler-proven escape-free loop iterations (watermark resets), and
user-declared [`region:` blocks](appendix-performance.md) whose value escapes
by copy-out. Hot paths
avoid allocating at all: an ownership analysis proves where accumulation
(`xs.push(e)`, `s = s + p`, `d.insert(k, v)`,
`x = f(move x)`) can mutate in place — aliases cost one copy where they
happen, never the whole loop — and dicts carry a hidden hash index. The result
runs at native-class throughput: a string builder stays linear instead of
O(n²), and list/dict/compute loops carry no per-iteration allocation overhead —
all inside the sandbox (`bench/BASELINE.md` has the numbers). The repository's
`spec/architecture.md` has the full
memory-model story and the honest list of current limitations.

That's the engine room. One more practical chapter: proving your own programs
correct.
