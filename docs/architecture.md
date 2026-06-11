# Architecture

How the witchy implementation fits together, and the engineering discipline
that holds it together. For the language itself see [language.md](language.md);
for the security model see [capabilities.md](capabilities.md).

## The pipeline

```
source ──lexer──> tokens ──parser──> AST ──linker──> one flat module
                                                  │
                                          consts/aliases inlined,
                                          traits desugared (traits.rs),
                                          sugar lowered (ranges, UFCS, ...)
                                                  │
                                              typeck.rs
                                                  │
                  ┌───────────────┴───────────────┐
             interpreter.rs                   codegen.rs
             (tree-walking,                   (hand-emitted WAT,
              the REFERENCE)                   run on wasmtime)
```

| File | Role |
|---|---|
| `src/lexer.rs` | Tokens, the off-side (indentation) layout pass, string interpolation, duration literals |
| `src/parser.rs` | Recursive descent with a Pratt expression core; sugar lowering (ranges, `xs[i]`, method calls, comprehensions) |
| `src/linker.rs` | Combines modules into one flat module with qualified names; bundles the std library (`include_str!`) |
| `src/typeck.rs` | Annotation-driven checking + HM unification (occurs-checked); capability rights; exhaustiveness; actor message validation |
| `src/traits.rs` | Trait desugaring to plain functions; monomorphization of bounded AND unbounded generics for the compiled backends |
| `src/interpreter.rs` | The reference semantics; also the `Dir`/`Net` confinement logic the sandbox reuses |
| `src/codegen.rs` | WAT emission: universal 8-byte value slots, per-shape structural-equality helpers, capability host imports |
| `src/runtime.rs` | The wasmtime sandbox: capability-gated host functions over one shared `ActorState`, memory caps, epoch preemption |
| `src/actor_system.rs` | Compiled actor programs: one VM per actor, per-kind capability gates, spawn-time Dir/Net handle translation, typed message routing |
| `src/capabilities.rs` | The footprint analyzer (`witchy caps`, `caps-diff`) — recomputed from source, never trusted metadata |
| `src/pm/` | The package manager: manifest/lockfile, resolution, content-addressed store, registry client/server, TUF, signing, the block-on-widening gate |
| `src/format.rs` | The canonical formatter (comment-preserving, round-trip-verified) |
| `src/lsp.rs` | Diagnostics language server |
| `std/` | The standard library, written in witchy |
| `projects/pm`, `projects/coven` | The package manager and registry, self-hosted in witchy |

## The parity discipline

The interpreter defines the semantics; the compiled backends must agree —
**zero silent divergence**, including error paths. This is the project's core
engineering invariant, enforced by:

- `witchy parity <file>`: runs both backends and compares; both-error counts
  as agreement, an error/value split is a reported divergence.
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

Capability values compile to **handles**: a `Dir` or `Net` is an i32 index
into a host-side table (paths and allowlists never enter guest memory, so a
module cannot forge or widen authority); `Console`/`Clock`/`Env` are erased
entirely (the linked host import *is* the authority).

## Memory model

The compiled backend is a **bump arena with structured reclamation** — no
tracing GC, no free lists; instead, memory is reclaimed at well-defined
lifetimes the compiler can prove (or the user declares):

- **Program exit** — the whole arena is discarded with the VM; linear memory
  is capped (1 GiB ceiling for `run`), so a runaway program traps rather than
  consuming the host.
- **Per message** (actors) — the host calls `__msg_prep` before each
  delivery, resetting the actor's arena to its base. Persistent state lives
  in host cells / globals, so a resident actor's memory is flat across
  millions of messages.
- **Per loop iteration** — escape-free loops get a watermark reset: the
  compiler proves nothing allocated inside the body outlives the iteration
  and rewinds the heap each pass.
- **`region:` blocks** — user-declared allocation scopes. Everything born in
  the region dies at its end; the block's VALUE escapes by a shape-directed
  copy-out that short-circuits on parent-side data (a passthrough result
  copies zero bytes — asserted in tests via the exported
  `__region_copy_bytes` counter). See [regions.md](regions.md).

On top of reclamation, hot mutation paths avoid allocating at all: the
linear-update optimizer turns unaliased self-assign shapes
(`xs = push(xs, e)`, `s = s <> p`, `d = insert(d, k, v)`) into in-place
appends with capacity doubling, and dicts carry a hidden open-addressing hash
index for O(1) lookups. The interpreter applies the same in-place
self-assign optimization (values are fully owned there, so the slot is the
value's only home). Measured: string workloads run 4–5.7× faster than Go,
lists/dicts/compute at parity — see `bench/BASELINE.md`.

## The runtime sandbox

Each VM is a wasmtime `Store` with its own linear memory and its own
`Linker`. A capability grant means "this host function is linked"; everything
else is structurally absent — a module importing an ungranted function fails
at instantiation, before any code runs. Resource bounds: a per-VM linear
memory cap and (under the scheduler) epoch-based preemption at loop back-edges
to reclaim runaway actors.

Compiled ACTOR programs run on the same gated surface
(`link_capability_imports` over the shared `ActorState`): each actor kind's
VM links only the import families its declared capability fields entitle it
to — an actor without a `Console` field physically has no `print` import,
and a `Dir[Read]` field links the read family only. `Dir`/`Net` fields are
i32 handles into the actor's own host-side table; `spawn` TRANSLATES the
spawner's handle into the spawnee's table (paths/allowlists never enter
guest memory), so attenuation (`subdir`, `restrict`) carries across VM
boundaries. The driver (`main`) VM takes its grant from the host: the dev
grant for `run`/`parity`, the computed footprint for `witchy sandbox`.

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
- `spawn` IS guest-callable from a compiled program's `main` AND from
  handlers: each `spawn` instantiates the actor's VM through a host import
  under the kind's own capability gate (Subject ids and Dir/Net handles
  travel as i32s and are translated; Console/Clock/Env are erased — the gate
  carries them), and `send` routes through the system. Messages between
  compiled actor VMs carry `Int`, `Float`, `String` (copied by content),
  `Subject` (delegating send authority), `List(Int)`/`List(String)`,
  scalar-tuple, and record fields. Anything outside that surface — including
  a capability-typed MESSAGE parameter (pass capabilities at `spawn`, not in
  messages) and `Secret`-typed actor fields — is a loud compile error, never
  a silent difference.
- The LSP has diagnostics, completion, and hover — no go-to-definition or
  rename yet.
- No tracing GC: reclamation is structural (see the memory model above). A
  long-running loop that accumulates into a single ever-growing value still
  grows; bound it with a `region:` or per-message state.
