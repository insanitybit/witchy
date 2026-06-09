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
        ┌─────────────────────────┬───────────────┴───────────────┐
   interpreter.rs            codegen.rs                      rustgen.rs
   (tree-walking,            (hand-emitted WAT,              (Rust source,
    the REFERENCE)            run on wasmtime)                rustc/LLVM)
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
| `src/runtime.rs` | The wasmtime sandbox: one `Store` per actor, capability-gated host functions, memory caps, epoch preemption |
| `src/rustgen.rs` | The native backend: transpile to Rust, compile with rustc |
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

## Memory model — honest limitations

The compiled backend uses a **bump allocator with no reclamation**: `$heap`
grows monotonically; values are never freed. This is a deliberate simplicity
choice with real consequences:

- **Fine:** CLI invocations, tests, build steps, request-scoped work — the
  whole arena is discarded when the VM exits, and linear memory is capped
  (1 GiB ceiling for `run`, smaller per-actor caps under the scheduler), so a
  runaway program traps rather than consuming the host.
- **Not fine:** long-running, allocation-heavy servers compiled to WASM. A
  loop that allocates indefinitely will eventually hit the memory cap and
  trap. Run resident services on the interpreter or native backend, or
  restart actors periodically (one actor = one VM = one arena).

GC or arena-reset-per-message is future work; the design (per-actor isolated
memories) is chosen so reclamation can land per-actor without a global
collector.

## The runtime sandbox

Each actor is a wasmtime `Store` with its own linear memory and its own
`Linker`. A capability grant means "this host function is linked"; everything
else is structurally absent — an actor importing an ungranted function fails
at instantiation, before any code runs. Resource bounds: a per-actor linear
memory cap and (under the scheduler) epoch-based preemption at loop back-edges
to reclaim runaway actors.

Trusted computing base: the lexer-to-codegen pipeline, the runtime host
functions, and wasmtime itself. Microarchitectural side channels (Spectre
class) are out of scope.

## Known gaps

Tracked honestly rather than hidden:

- `Result` (multi-parameter generic) `==` is a compile error, not structural —
  instantiating both payload types needs type information codegen doesn't
  carry yet. (Single-parameter generics like `Option` compare structurally.)
- `spawn` inside compiled programs is host-driven (the demo scheduler), not a
  guest-callable import yet: messages between compiled actor VMs currently
  carry scalar fields only, and guest-initiated spawn needs a cross-VM value
  marshaling ABI. Loud compile error, never a silent difference; the
  interpreter runs the full actor model.
- The LSP has diagnostics, completion, and hover — no go-to-definition or
  rename yet.
- No GC (see the memory model above).
