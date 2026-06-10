# Under the Hood: Three Backends, One Meaning

witchy runs three ways, and understanding why — and what holds them together —
explains a lot of the language's design choices.

| Backend | How | For |
|---|---|---|
| **Interpreter** | tree-walking, in Rust | development; the *reference* semantics |
| **WebAssembly** | hand-emitted WAT on wasmtime | confinement; the browser playground |
| **Native** | transpiled to Rust, compiled by `rustc` | speed |

```sh
witchy program.witchy            # interpreter
witchy sandbox program.witchy    # WebAssembly
witchy native program.witchy     # native
```

## Parity: the invariant that makes it safe

These aren't three languages that happen to look alike. They are three
implementations of *one* language, held to a single rule:

> A program produces **identical** output on every backend — including identical
> failure behavior — or it doesn't compile.

The interpreter is the reference: it defines what a program *means*. The other
two must match it. The project's test suite contains hundreds of *differential*
tests that run a program on the interpreter and the compiled backend and assert
the outputs are equal, plus a property-based fuzzer generating programs to try
to pry them apart. You can run the check on any program yourself:

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
stayed inside the portable language. A handful of conveniences are
interpreter-only — and the compiler tells you, loudly, the moment you lean on one
in code meant for the sandbox. (It never silently mis-renders: `to_string` of a
whole list, tuple, record, ADT, or dict now compiles and renders identically on
all three backends, and a shape the compiler genuinely can't resolve is a clear
error, not a wrong answer.) You're never guessing.

## A note on memory

The compiled backends use a simple bump allocator with no garbage collection:
memory grows and is reclaimed all at once when the program (or actor) exits.
This is ideal for the things witchy compiles for — command-line tools, build
steps, request-scoped work, sandboxed plugins — where the whole arena is
discarded at the end and a cap bounds runaway growth. A long-running,
allocation-heavy server compiled to WebAssembly would eventually hit that cap;
run those on the interpreter or native backend, or structure them so each unit
of work is its own short-lived actor. The repository's `docs/architecture.md` is
candid about this and the other current limitations.

That's the engine room. One more practical chapter: proving your own programs
correct.
