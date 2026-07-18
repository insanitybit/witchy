# Architecture

How the witchy implementation fits together, and the engineering discipline
that holds it together. For the language itself see [language.md](language.md);
for the security model see [capabilities.md](capabilities.md).

## The pipeline

```
source ──lexer──> tokens ──parser──> AST ──linker──> one flat module
                                                  │
                                          consts/aliases inlined,
                                          records/traits/async/gen lowered,
                                          sugar lowered (ranges, UFCS, ...)
                                                  │
                                              typeck.rs
                                                  │
                  ┌───────────────────────────────┴───────────────┐
             codegen/ ───> WIR ──> wasm-encoder            interpreter.rs
             (the run path: lowered to a structured         (tree-walking;
              IR then encoded to a wasm binary,              the parity ORACLE,
              run on wasmtime)                               not a user run path)
```

There is **one user-program run path: the compiled WASM backend.** `witchy
<file>`, `witchy run`, and `witchy sandbox` all compile to a wasm binary and
execute it under wasmtime, so dev == deploy by construction. The tree-walking
interpreter is *not* a run path — it is the differential oracle (`witchy
parity`), the `comptime:` evaluator, the in-language test runner, and the
effectful build-step executor.

Compile-time generation has a parallel, compiler-only provenance result. The
ordinary `link` API returns only the expanded `Module`, so type checking and
both execution backends continue to consume one identical AST. Tooling may use
`link_with_origins` to retain typed generated-node IDs, structural AST paths,
definition and invocation spans, and ordered syntax-hole ancestry. This table
is allocated and remapped by compiler passes; formatted source is never a node
identity.

### Workspace layout ([RFC-0018](../rfcs/0018-compiler-architecture.md))

The compiler is a **Cargo workspace**: seven stage-aligned library crates under
`crates/`, plus the `witchy` binary package (the CLI, the wasm-playground
`cdylib`, and the native-only LSP/PM/idp tooling). Crate privacy makes the stage
boundaries **compiler-enforced** — a pass cannot reach into another stage's
internals. The crate graph is a DAG rooted at `witchy-syntax` and `witchy-wir`;
everything flows downstream to the binary. To change a stage, edit its crate.

| Crate | Modules | Role |
|---|---|---|
| `witchy-syntax` | `lexer`, `parser`, `ast`, `format`, `origin`, + the AST-level base passes (`aliases`, `consts`, `fmt`, `async_lower`, `generators`, `optimize`, `reflect`, `derive`, `records`, `doc`, `linker`, `lambda_scan`, `build_entry`) | Source → AST (off-side layout, interpolation, duration literals; Pratt-core parser + sugar lowering), the canonical formatter, and the front-end/base layer every later stage builds on. `origin` defines the typed generated-node/span side table; `linker` combines modules into one flat module with qualified names + bundles the std library (`include_str!`). |
| `witchy-types` | `typeck`, `traits` | Annotation-driven checking + HM unification (occurs-checked), capability rights, exhaustiveness; trait desugaring to plain functions + monomorphization of bounded AND unbounded generics. Mutually recursive — one crate. |
| `witchy-wir` | `wir`, `wir_opt`, `wir_prelude`, `wir_encode` | The structured IR (typed expression tree, named lexical `Block`/`Loop` labels, no relooper), the peephole pass (cancels redundant slot/kind round-trips), the precompiled runtime-helper prelude (lists/strings/dicts/crypto), and the `wasm-encoder` backend. |
| `witchy-lower` | `codegen`, `analysis` | Lowers the checked AST to WIR (universal 8-byte value slots, per-shape structural-equality helpers, capability host imports); `analysis` is the uniqueness / cap-token pass the in-place fast paths depend on. |
| `witchy-runtime` | `value`, `native`, `net`, `confine`, `runtime` *(native-only)* | The runtime `Value` (shared by interpreter + host), the native-function registry (FFI-as-capability), address/path confinement (`..`/absolute/symlink rejection, address-set policy — shared by both backends), and the wasmtime sandbox (capability-gated host functions, memory caps, epoch preemption). The first four are wasm-safe; `runtime` sits behind the `native` feature. |
| `witchy-interp` | `interpreter`, `comptime`, `tagged`, `pipeline` | The tree-walking reference semantics — the parity ORACLE (`witchy parity`, `comptime`, test runner, build steps; *not* a user run path) — plus compile-time `comptime:` / `tag"…"` evaluation and the linker's injected compile-time expander. |
| `witchy-caps` | `capabilities`, `grants` | The footprint analyzer (`witchy caps`, `caps-diff`) — recomputed from source, never trusted metadata — and grant-document (`--grants` TOML) parsing + cross-check (`witchy grants-check`). |
| `witchy` *(binary)* | `main`, `lib` (the wasm-playground `cdylib`), `lsp`, `idp` | The thin CLI that wires the stages; the in-browser playground entry; the diagnostics LSP (diagnostics/completion/hover); and the trusted-publishing IdP *test* simulator (`coven-gen-issuer`/`coven-mint-token`) standing in for an external CI identity provider. |

`std/` is the standard library, written in witchy; `projects/pm`, `projects/coven`
are the package manager and registry, self-hosted in witchy.

## The parity discipline

The interpreter defines the semantics; the compiled backend must agree —
**zero silent divergence**, including error paths. This is the project's core
engineering invariant, enforced by:

- `witchy parity <file>`: runs both backends and compares values and complete
  error diagnostics byte-for-byte. An error/value split, different errors, or a
  missing compiled source location is a reported divergence.
- Hundreds of differential unit tests plus property-based tests (proptest).
- A CI sweep running `parity` over every example.

The consequence for contributors: **any observable behavior you add must land
on both backends in the same change**, or be a loud error on the one that
lacks it. The codebase treats a "documented divergence" as a bug.

## The WASM value model

Compiled witchy uses untyped 8-byte slots: every collection element, record
field, and closure argument is an i64 slot (`to_slot`/`from_slot` convert —
Ints live as i64, Floats bit-reinterpreted, pointers/Bools sign-extended i32).
Because slots are untyped at runtime, type-directed code generation does the
work types would: structural equality, for instance, derives an `EqShape` from
the static types and generates a memoized helper function per shape.

Migrated capability values compile to opaque `externref`s, not integer slots:
`Dir`, `File`, `Net`, `Socket`, `Listener`, and `Secret` are host-rooted
authority objects whose paths, allowlists, streams, listeners, and secret bytes
never enter guest memory unless an explicit API such as `crypto.reveal` returns
data. Code cannot mint or widen them, and corrupted linear memory cannot forge
one. Migrated capabilities may cross only boundaries with a typed reference
representation: directly, as nullable `Option(reference)` where the ABI carries
a null reference, as transparent single-field capability brands represented by
a direct externref, inside a fully concrete structural tuple or closed nominal
instance lowered to a Wasm GC struct, or in a closure's per-lambda GC
environment. The nominal category includes generic and non-generic sealed
capabilities, plain
named-field records, positional wrappers, and multi-variant sums. Capability
tuples are interned by their recursively typed field shape; qualifiers do not
change that shape, and tuples may nest in other tuples or nominal GC
aggregates. A sum stores its tag and each variant's payload in disjoint typed
field bands, with inactive reference fields null; mixed and recursive nesting
stays reference-typed. Closed `Result` values use the same sum representation,
and every reference-bearing `List(T)` uses an array of its exact reference kind.
Typed-array operations include persistent updates, full-width bounds-checked
reads, list-pattern tails, and `pop` extraction with ordinary `var` write-back.
`Dict` reference payloads, open generic function ABIs, region copy-out, and
isolated-worker crossings remain rejected until those boundaries have fixed
typed representations.
`Console`/`Clock`/`Rand`/`Env`/`Exec`/`SecretStore` and build capabilities are
zero-representation authorities: type checking requires the source value, while
the linked host import and launch grant carry the runtime authority.

## Memory model

The compiled backend is a **bump arena with structured reclamation** — no
tracing GC, no free lists; instead, memory is reclaimed at well-defined
lifetimes the compiler can prove (or the user declares):

- **Program exit** — the whole arena is discarded with the VM; linear memory
  is capped (1 GiB ceiling for `run`), so a runaway program traps rather than
  consuming the host.
- **Per loop iteration** — escape-free loops get a watermark reset: the
  compiler proves nothing allocated inside the body outlives the iteration
  and rewinds the heap each pass.
- **`region:` blocks** — user-declared allocation scopes. Everything born in
  the region dies at its end; the block's VALUE escapes by a shape-directed
  copy-out that short-circuits on parent-side data (a passthrough result
  copies zero bytes — asserted in tests via the exported
  `__region_copy_bytes` counter). See [regions.md](../rfcs/regions.md).

On top of reclamation, hot mutation paths avoid allocating at all. The
**uniqueness pass** (`crates/witchy-lower/src/analysis.rs`, design in
[ownership-analysis.md](../rfcs/ownership-analysis.md)) drives in-place mutation of
typed `var` accumulation (`xs.push(e)`, `d.insert(k, v)`) and ordinary linear
updates (`s = s <> p`, `x = f(move x)`) through a runtime ownership
token: the analysis finds every statement that can create a live whole-alias
(the token is zeroed there — path-sensitively) and every site whose own RHS
embeds one; everything provably unaliased mutates in place with capacity
doubling. Function summaries (a bottom-up pass over the call graph) mean a
read-only helper call doesn't break the chain, `let`-borrow parameters are
certified by typeck, and an `own` parameter carries the token ACROSS the
call (`x = grow(move x)` pipelines are O(n) end to end). Dicts carry a
hidden open-addressing hash index for O(1) lookups. The interpreter applies
the same in-place self-assign optimization (values are fully owned there).
Accumulation that falls back to copying inside a loop is flagged at check
time (`witchy check` notes + LSP hints); `WITCHY_OPT=-inplace` compiles the
copying paths for differential verification, and the exported
`__witchy_reowns` counter lets tests assert copy counts. Measured against the
benchmark baseline: string workloads run at 4–5.7× the reference throughput,
lists/dicts/compute at parity — see `bench/BASELINE.md`.

## The runtime sandbox

Each program is one wasmtime `Store` (one VM) with its own linear memory and
its own `Linker`. A capability grant means "this host function is linked";
everything else is structurally absent — a module importing an ungranted
function fails at instantiation, before any code runs.
`link_capability_imports` (over the shared `VmState`) defines only the
families the grant entitles: a program with no `Console` in its footprint
physically has no `print` import, and a `Dir[Read]` footprint links the read
family only. Migrated host-held capabilities (`Dir`, `File`, `Net`, `Socket`,
`Listener`, `Secret`) compile to opaque `externref` values carrying host-side
authority objects, so paths, allowlists, streams, listeners, and secret bytes
never enter guest memory and cannot be forged by corrupting an integer handle.
Attenuation
(`dir.subtree`, `dir.read_file`/`write_file`, `net.only`/`net.deny`) mints a
narrower host object and returns a new externref. Zero-representation
capabilities enforce authority by the same structural import gating without
passing a runtime handle. The
grant comes from the host: the dev grant for `run`/`parity`, the computed
footprint for `witchy sandbox`. Resource bounds: a per-VM linear memory cap
and (under the scheduler) epoch-based preemption at loop back-edges to reclaim
a runaway guest.

Concurrency lives *inside* that single VM. `async`/`await`, `spawn`, and
channels lower to a cooperative executor written in pure witchy (`std/task`;
`std/chan` provides the channel surface), so concurrent tasks share one linear
memory and one capability grant rather than running as separate VMs — and because
the scheduler is ordinary witchy code, a concurrent run is byte-identical on both
backends. See
[concurrency-design.md](../rfcs/concurrency-design.md).

Trusted computing base: the lexer-to-codegen pipeline, the runtime host
functions, and wasmtime itself. Microarchitectural side channels (Spectre
class) are out of scope.

## Known gaps

Tracked honestly rather than hidden:

- Generic ADT `==` (including `Result`) is structural when the payload types
  are visible at the comparison site (declared parameter or return types,
  constructor literals). A payload codegen cannot resolve — e.g. through an
  unspecialized generic function, or a *recursive* generic ADT — stays a loud
  compile error, never a silent pointer compare.
- Spawned tasks return `Nil` and report results over channels (the Go model):
  there is no typed `JoinHandle(T)` — one would force a native runtime and
  break the byte-identical executor, so the structured forms (`chan.scope`,
  `chan.gather`, `chan.par_map`) cover the join-with-result shapes. `await`
  is supported in loop bodies, including `while` bodies that carry mutable
  locals across the await; it is still not supported in loop/branch
  conditions or match scrutinees. See
  [concurrency-design.md](../rfcs/concurrency-design.md).
- The LSP has diagnostics, completion, hover, and expansion-aware document
  symbols. It has no go-to-definition or rename yet.
- No tracing GC: reclamation is structural (see the memory model above). A
  long-running loop that accumulates into a single ever-growing value still
  grows; bound it with a `region:` or a per-iteration watermark.
