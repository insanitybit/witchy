# Architecture

How the witchy implementation fits together, and the engineering discipline
that holds it together. For the language itself see [language.md](language.md);
for the security model see [capabilities.md](capabilities.md).

## The pipeline

```
source ──lexer/parser──> per-module AST
                              │
                 checked link + expansion
                 (source checks, name resolution,
                  destructive source lowering)
                              │
                         CheckedModule
                    (AST + origins + declarations
                     + authenticated owners, when loaded)
                              │
                   ┌──────────┴──────────┐
             checked lowering       checked interpreter
                    │                (parity/comptime/
                   WIR                tests/build only)
                    │
               wasm-encoder
                    │
             wasmtime execution
```

`witchy-types::pipeline::CheckedModule` is the production proof boundary. The
checked-link service in `witchy-interp::pipeline` supplies the compile-time
expander to `witchy-types::pipeline`, which owns the link/source-check/type-check
sequence. Production lowering, compilation, and interpreter runners accept that
proof rather than a bare AST. Raw-module sinks used by compiler rejection tests
are absent from normal builds and exist only behind explicit test features.

`CheckedModule::module()` is a read-only compiler-stage view, not a capability
boundary: the AST remains cloneable. The invariant is therefore enforced at
production sinks, which require the proof object. Synthetic compiler-owned
modules must re-enter through the explicit synthetic checker rather than assert
that a transformed AST remains checked.

There is **one user-program run path: the compiled WASM backend.** `witchy
<file>`, `witchy run`, and `witchy sandbox` all compile to a wasm binary and
execute it under wasmtime, so dev == deploy by construction. The tree-walking
interpreter is *not* a run path - it's the differential oracle (`witchy
parity`), the `comptime:` evaluator, the in-language test runner, and the
effectful build-step executor.

Compile-time generation retains a compiler-only provenance result inside the
checked artifact. Tooling that must operate on an incomplete, type-invalid
buffer may stop earlier at `link_with_origins`; that API returns a named
`LinkedModule` phase artifact rather than authorizing execution. It retains
typed generated-node IDs, structural AST paths, definition and invocation
spans, and ordered syntax-hole ancestry. This table is allocated and remapped
by compiler passes; formatted source is never a node identity.

### Workspace layout ([RFC-0018](../rfcs/0018-compiler-architecture.md))

The compiler is a **Cargo workspace**: eleven stage-aligned library crates under
`crates/`, plus the `witchy` root package (the CLI, the wasm-playground
`cdylib`, and the native-only LSP/PM/idp tooling). Package dependencies enforce
the coarse stage DAG. Some stage internals and migration re-exports remain
public within the workspace, so module-level interfaces aren't yet uniformly
narrow. The executable dependency ceiling and the remaining decomposition work
are tracked in the [architecture and redundancy ledger](architecture-ledger.md).

| Crate | Modules | Role |
|---|---|---|
| `witchy-cap-model` | dependency-bottom capability catalog | Canonical capability names, classes, arities, rights, and host-operation vocabulary shared by compiler, analyzers, fixtures, and runtime without depending on syntax or policy. |
| `witchy-testkit` | `basic`, `engine`, `filesystem`, `fetch`, `exec`, `secret`, `model`, `validate` | Backend-neutral deterministic fixture plans, provider state machines, expectations, and transcripts. It carries no compiler or runtime authority. |
| `witchy-test-host` | opaque fixture-session dispatch | The shared fixture host used by interpreter, Wasmtime, and browser adapters; backend adapters consume it rather than reimplementing fixture semantics. |
| `witchy-syntax` | `lexer`, `parser`, `ast`, `format`, `origin`, + the AST-level base passes (`aliases`, `consts`, `fmt`, `async_lower`, `generators`, `optimize`, `reflect`, `derive`, `records`, `doc`, `linker`, `lambda_scan`, `build_entry`) | Source → AST (off-side layout, interpolation, duration literals; Pratt-core parser + sugar lowering), the canonical formatter, and the front-end/base layer every later stage builds on. `origin` defines the typed generated-node/span side table; `linker` combines modules into one flat module with qualified names + bundles the std library (`include_str!`). |
| `witchy-types` | `pipeline`, `typeck`, `traits`, `runtime_type` | The checked front-end proof boundary; annotation-driven checking + HM unification (occurs-checked), capability rights, exhaustiveness; trait desugaring to plain functions + monomorphization; authenticated runtime declaration identities and closed shapes. |
| `witchy-wir` | `layout`, `wir`, `wir_opt`, `wir_prelude`, `wir_encode` | Canonical specialized layout descriptors and transport, the structured IR (typed expression tree, named lexical `Block`/`Loop` labels, no relooper), the peephole pass, the precompiled runtime-helper prelude, and the `wasm-encoder` backend. |
| `witchy-lower` | `codegen`, `analysis` | Lowers the checked AST to WIR, selecting ordinary slots, typed references, or canonical specialized layouts before Wasm-kind erasure; `analysis` is the uniqueness / cap-token pass the in-place and destination paths depend on. |
| `witchy-confinement` | normalized policy plus platform providers | Target-neutral filesystem, network, Fetch-origin, and syscall-class confinement policy; Linux Landlock/seccomp enforcement consumes this policy without depending on compiler stages or Wasmtime. |
| `witchy-runtime` | `value`, `native`, `net`, `confine`, `runtime` *(native-only)* | The runtime `Value` (shared by interpreter + host), native-function registry (FFI-as-capability), runtime adapters over shared confinement policy, and the Wasmtime sandbox (capability-gated host functions, memory caps, epoch preemption). The non-`runtime` modules are wasm-safe; `runtime` sits behind the `native` feature. |
| `witchy-interp` | `interpreter`, `comptime`, `tagged`, `pipeline` | The tree-walking reference semantics - the parity ORACLE (`witchy parity`, `comptime`, test runner, build steps; *not* a user run path) - plus compile-time `comptime:` / `tag"…"` evaluation and the task-shaped checked-link service that injects the compile-time expander. |
| `witchy-caps` | `capabilities`, `grants` | The footprint analyzer (`witchy caps`, `caps-diff`) - recomputed from source, never trusted metadata - and grant-document (`--grants` TOML) parsing + cross-check (`witchy grants-check`). |
| `witchy` *(root package)* | `main`, `cli`, `source`, `lib` (the wasm-playground `cdylib`), `lsp`, `idp` | The composition package: browser entrypoints, diagnostics LSP, trusted-publishing IdP *test* simulator, and native CLI orchestration. `cli` owns help/version presentation and shared flag/secret decoding; `source` owns native project discovery, bundled lookup, dependency-aware file loading/linking, and source expansion. Dispatch and command execution remain concentrated in `main.rs` and are tracked in the architecture ledger rather than described here as already thin. |

`std/` is the standard library, written in witchy; `projects/pm`, `projects/coven`
are the package manager and registry, self-hosted in witchy.

## The parity discipline

The interpreter defines the semantics; the compiled backend must agree -
**zero silent divergence**, including error paths. This is the project's core
engineering invariant, enforced by:

- `witchy parity <file>`: runs both backends and compares values and complete
  error diagnostics byte-for-byte. An error/value split, different errors, or a
  missing compiled source location is a reported divergence.
- Hundreds of differential unit tests plus property-based tests (proptest).
- A CI sweep running `parity` over every example.
- `tests/misc/semantic_conformance.rs`: independently stated exact values,
  rejection diagnostics, and capability footprints. Its positive control
  injects a shared semantic mutation that both backends agree on and proves the
  external expectation still rejects it.

The consequence for contributors: **any observable behavior you add must land
on both backends in the same change**, or be a loud error on the one that
lacks it. The codebase treats a "documented divergence" as a bug.

Parity isn't a specification oracle: the interpreter and compiled backend
share parsing, linking, type checking, and parts of lowering policy, so a
common-mode defect can preserve agreement. New semantics therefore need an
independent expected result or rejection in addition to parity. The
conformance corpus is deliberately small and reviewable; broad generated
coverage stays in the differential and property suites.

## The WASM value model

The compiled backend has three physical value families. Ordinary linear-memory
collections and aggregates retain the 8-byte slot representation
(`to_slot`/`from_slot` convert Ints, Float bits, pointers, and Bools). Concrete
reference-bearing values use typed Wasm references as described below. A closed
declared-`packed` record, a tuple containing a packed component, a `List` of such
elements, or a fixed-layout packed sum instead uses its canonical RFC-0111
`LayoutId`. The versioned descriptor fixes scalar widths, alignment, offsets,
list stride and header state, sum tag/payload bands, ownership positions, and
derived operation shapes before WIR encoding. Layout IDs and their canonical
descriptor graph are embedded once in the artifact's `witchy.layouts` section;
the runtime validates the schema, hashes, dependencies, roots, and generated host
contracts before instantiation.

The shipped specialized-boundary matrix is deliberately narrower than the
descriptor vocabulary:

| Boundary | Compiled behavior for a declared specialized value |
|---|---|
| Local construction, field/index access, packed-list traversal and mutation, and fixed-sum matching | Uses descriptor offsets, stride, tag width, and payload bands. No per-element record boxes are introduced. |
| Direct named function calls and linked user-module calls | Packed records, packed lists, packed-containing tuples, and fixed-layout packed sums cross as the exact descriptor-shaped pointer ABI. Parameter/result ownership still comes from the checked RFC-0110 access envelope. |
| Direct generic calls | Each closed instance is keyed by logical type identity, the RFC-0110 access envelope, exact parameter/result `LayoutId`s, and the optimization schema. Packed construction, indexed traversal, mutation, recursive/direct helper calls, and return retain that instance; open or unsupported crossings don't guess a layout. |
| Function values, lambdas/closure captures, and trait/existential calls | Reject before the legacy indirect ABI can box or reshape the value. Exact callable-layout diagnostics name the `LayoutId`; there's no packed indirect-call ABI yet. |
| Host import boundary | ABI version 8 authenticates an accepted-`LayoutId` set against `witchy.layouts`. Every production import currently publishes an empty set, so a structured specialized crossing rejects. A future marshal must name its accepted descriptor and reshaped-byte counter. Capability references remain `externref` and can never be inline scalar fields. |
| `region:` result, isolated worker, channel, and other unsupported dynamic boundaries | Reject rather than copying through the universal-slot path. Artifact descriptor transport doesn't by itself authorize packed value transport between VMs. |
| Whole-value equality | Fixed-layout packed sums with derived/default structural equality use their descriptor's equality operation, tag width, variant child layouts, and physical payload offsets. Custom `PartialEq`, other specialized equality, and all specialized rendering remain fail-closed. |

Destination passing is also descriptor-gated. A private direct function whose
successful paths all construct the same fixed result may receive a hidden
destination. The current admitted producers are an exact `unique` packed-record
result written into compatible dead caller storage and a fixed packed sum written
into a proven nonescaping immediate-consumer scratch. Public entry points,
own/`var` capacity envelopes, nested allocating payloads, incomplete constructor
returns, escaping old values, and layout mismatches retain the allocating path.
The observable value and write-back order don't change.

RC-header elision has one conservative admitted class: in `mode opt`, with
`unbox` and `rc-elide` enabled, a nonempty immutable local `List(Packed)` may use
an `RcHeader::Elided` descriptor only when a whole-module scan finds no signature,
whole-value boundary call or return, alias, mutation, nested scope, dynamic
wrapper, or active loan for that exact list type. Every other packed list uses
`RcHeader::Required`.
`witchy stats` exposes `destination_candidates_forwarded`,
`packed_alloc_calls`, and `packed_alloc_bytes`; generated modules also export
the test-visible `__witchy_rc_headers_emitted` and
`__witchy_rc_headers_elided` counters.

Ordinary slot-based equality and rendering helpers are still derived from the
checked logical type. Derived/default structural `==`/`!=` on a fixed-layout
packed sum instead consumes the canonical descriptor's equality operation and
physical variant layout. Custom `PartialEq`, other specialized whole-value
equality, rendering, region copy-out, and serialization remain fail-closed until
those consumers are descriptor-driven.

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
tuples are interned by their recursively typed field shape; qualifiers don't
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

The compiled backend is a **bump arena with structured reclamation** - no
tracing GC, no free lists; instead, memory is reclaimed at well-defined
lifetimes the compiler can prove (or the user declares):

- **Program exit** - the whole arena is discarded with the VM; linear memory
  is capped (1 GiB ceiling for `run`), so a runaway program traps rather than
  consuming the host.
- **Per loop iteration** - escape-free loops get a watermark reset: the
  compiler proves nothing allocated inside the body outlives the iteration
  and rewinds the heap each pass.
- **`region:` blocks** - user-declared allocation scopes. Everything born in
  the region dies at its end; the block's VALUE escapes by a shape-directed
  copy-out that short-circuits on parent-side data (a passthrough result
  copies zero bytes - asserted in tests via the exported
  `__region_copy_bytes` counter). See [regions.md](../rfcs/regions.md).

On top of reclamation, hot mutation paths avoid allocating at all. The
**uniqueness pass** (`crates/witchy-lower/src/analysis.rs`, design in
[ownership-analysis.md](../rfcs/ownership-analysis.md)) drives in-place mutation of
typed `var` accumulation (`xs.push(e)`, `d.insert(k, v)`) and ordinary linear
updates (`s = s <> p`, `x = f(move x)`) through a runtime ownership
token: the analysis finds every statement that can create a live whole-alias
(the token is zeroed there - path-sensitively) and every site whose own RHS
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
lists/dicts/compute at parity - see the [benchmark baseline](../bench/BASELINE.md).

## The runtime sandbox

Each program is one wasmtime `Store` (one VM) with its own linear memory and
its own `Linker`. A capability grant means "this host function is linked";
everything else is structurally absent - a module importing an ungranted
function fails at instantiation, before any code runs.
`link_capability_imports` (over the shared `VmState`) defines only the
families the grant entitles: a program with no `Console` in its footprint
physically has no `print` import, and a `Dir[Read]` footprint links the read
family only. Migrated host-held capabilities (`Dir`, `File`, `Net`, `Socket`,
`Listener`, `Secret`) compile to opaque `externref` values carrying host-side
authority objects, so paths, allowlists, streams, listeners, and secret bytes
never enter guest memory and cannot be forged by corrupting an integer handle.
Narrowing (`dir.subtree`, `dir.read_file`/`write_file`, `net.only`/`net.deny`)
mints a narrower host object and returns a new externref. Zero-representation
capabilities enforce authority by the same structural import gating without
passing a runtime handle. The
grant comes from the host: the dev grant for `run`/`parity`, the computed
footprint for `witchy sandbox`. Resource bounds: a per-VM linear memory cap
and (under the scheduler) epoch-based preemption at loop back-edges to reclaim
a runaway guest.

Concurrency lives *inside* that single VM. `async`/`await`, `spawn`, and
channels lower to a cooperative executor written in pure witchy (`std/task`;
`std/chan` provides the channel surface), so concurrent tasks share one linear
memory and one capability grant rather than running as separate VMs - and because
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
  constructor literals). A payload codegen cannot resolve - e.g. through an
  unspecialized generic function, or a *recursive* generic ADT - stays a loud
  compile error, never a silent pointer compare.
- Spawned tasks return `Nil` and report results over channels (the Go model):
  there's no typed `JoinHandle(T)` - one would force a native runtime and
  break the byte-identical executor, so the structured forms (`chan.scope`,
  `chan.gather`, `chan.par_map`) cover the join-with-result shapes. `await`
  is supported in loop bodies, including `while` bodies that carry mutable
  locals across the await; it's still not supported in loop/branch
  conditions or match scrutinees. See
  [concurrency-design.md](../rfcs/concurrency-design.md).
- The LSP has diagnostics, completion, hover, and expansion-aware document
  symbols. It has no go-to-definition or rename yet.
- No tracing GC: reclamation is structural (see the memory model above). A
  long-running loop that accumulates into a single ever-growing value still
  grows; bound it with a `region:` or a per-iteration watermark.
