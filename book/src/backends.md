# Under the Hood: Two Backends, One Meaning

witchy runs two ways, and understanding why — and what holds them together —
explains a lot of the language's design choices.

| Backend | How | For |
|---|---|---|
| **Interpreter** | tree-walking, in Rust | development; the *reference* semantics |
| **WebAssembly** | hand-emitted WAT on wasmtime | deployment: confinement *and* speed; the browser playground |

```sh
witchy program.witchy            # interpreter
witchy sandbox program.witchy    # WebAssembly, granted exactly its footprint
```

The split is deliberate: the interpreter optimizes for being *obviously
correct* (it defines the semantics), the WASM tier for being *fast and
confined* (the capability boundary is the VM boundary — an ungranted host
function isn't denied, it simply does not exist in the instance). There used
to be a third, native backend (transpilation to Rust); it was retired once
the WASM tier reached native-class performance, because a third
implementation of the semantics bought speed witchy no longer needed at the
price of a wider parity surface.

## Parity: the invariant that makes it safe

These aren't two languages that happen to look alike. They are two
implementations of *one* language, held to a single rule:

> A program produces **identical** output on every backend — including identical
> failure behavior — or it doesn't compile.

The interpreter is the reference: it defines what a program *means*. The
compiled tier must match it. The project's test suite contains hundreds of
*differential* tests that run a program on the interpreter and the compiled
backend and assert the outputs are equal, plus a property-based fuzzer
generating programs to try to pry them apart. You can run the check on any
program yourself:

```sh
witchy parity program.witchy
```

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
plainly: **you develop against the interpreter and deploy to the VM, and they
are the same program.** Without parity, "it worked when I tested it" and "it does
the right thing in the sandbox" would be two separate hopes. With it, they're one
fact, mechanically enforced.

It also shapes how you write witchy. `witchy parity` is your signal that you've
stayed inside the portable language — and that language now covers the whole
standard library, including the native intrinsics (`crypto`, `encoding`, the
`compiler` module), networking, and **actor programs**: `witchy parity` runs an
actor program with each actor in its own VM, capabilities gated per actor, and
diffs the output against the interpreter. The few remaining edges (a
capability in a *message*, a `Secret`-typed actor field) are loud compile
errors, never silent differences. You're never guessing.

## A note on memory

The compiled tier has no garbage collector — and doesn't miss it. Memory is a
bump arena reclaimed at *structural* lifetimes: the program's exit, an
actor's per-message reset (state lives host-side, so a resident actor stays
flat across millions of messages), compiler-proven escape-free loop
iterations (watermark resets), and user-declared [`region:`
blocks](appendix-performance.md) whose value escapes by copy-out. Hot paths
avoid allocating at all: unaliased accumulation (`xs = push(xs, e)`,
`s = s <> p`, `d = insert(d, k, v)`) mutates in place, and dicts carry a
hidden hash index. The result benches at native-class speed — strings 4–5.7×
faster than Go, lists/dicts/compute at parity (`bench/BASELINE.md`) — while
staying a sandbox. The repository's `docs/architecture.md` has the full
memory-model story and the honest list of current limitations.

That's the engine room. One more practical chapter: proving your own programs
correct.
