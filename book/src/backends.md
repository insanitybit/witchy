# Execution Backends: One Meaning

witchy has one production run path and one independent semantic oracle. Their
agreement explains several of the language's design constraints.

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

The interpreter is kept direct enough to serve as the readable reference
semantics. The WebAssembly tier is the optimized, confined implementation: the
VM boundary is also the capability boundary, and an ungranted host function is
absent from the instance. A third backend would add another independent
implementation to the parity obligation, so the project maintains these two.

## Parity: the invariant that makes it safe

These aren't two languages that happen to look alike. They are two
implementations of *one* language, held to a single rule:

> Every supported program has identical results and failures on both backends.
> A construct that cannot meet that contract is rejected loudly.

The interpreter defines the result; the compiled tier must match it. Hundreds of
differential tests run the same source through both implementations. Property
tests generate additional programs, and CI runs the maintained examples through
`witchy parity`. The CLI exposes that same check so a user can inspect the
invariant on a particular program. Ordinary `run` does not secretly execute the
interpreter first; the test and parity harnesses are what keep the production
compiler honest.

Parity includes edge and failure cases:

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

**The program you run is the program you deploy**: `witchy run` and
`witchy sandbox` are the same compiled backend, differing only in how much
authority the host hands over. Differential coverage checks that the run path
implements the independent reference semantics, while the sandbox constrains
which host services that compiled module can reach.

There is no silently divergent portable subset. The concurrency model
(`async`/`await`, spawning, and channels) runs on a deterministic cooperative
executor written in witchy. Native services such as networking and cryptography
are host imports with shared semantics; a browser that does not supply such an
import cannot run that operation. When a representation or host cannot support a
construct faithfully, compilation or instantiation fails instead of selecting a
different meaning.

## Current limits

The loud boundaries are part of the current language state:

- Equality for a generic algebraic data type requires its payload types to be
  known at the comparison site. Unresolved and recursive generic cases are
  rejected rather than compared by pointer identity.
- `dyn Trait` syntax, identity, and existential-safety checks exist in the
  frontend, but existential values cannot yet be constructed or dispatched.
  Any program mentioning one ends with a single feature-stage error before
  backend lowering; use monomorphized generics or `impl Trait` today.
- Capability-bearing values work directly and in closed tuples, nominal types,
  closures, `Option`, `Result`, and typed lists. Capability-bearing `Dict`
  entries, open generic call boundaries, `region:` copy-out, and isolated-worker
  callbacks remain rejected until they have fixed typed representations.
- `await` works in loop bodies and may carry mutable locals, but not in branch or
  loop conditions or in match scrutinees. Spawned tasks return `Nil`; structured
  combinators and channels carry results.
- The current compiled async executor is intended for bounded task graphs and
  streams. Long-lived producer/consumer loops eventually exhaust the linear
  arena; the [async chapter](tour-async.md) gives the operational ceiling.
- Browser hosts grant only the capability families they implement. The default
  book host is pure-compute plus `Console`; native `Exec`, secrets, and raw
  sockets are unavailable there.

These are checked boundaries, not alternate semantics. The implementation's
current list is maintained in
[`spec/architecture.md`](https://github.com/insanitybit/witchy/blob/master/spec/architecture.md).

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

The next chapter covers source-level conventions, followed by the test runner.
