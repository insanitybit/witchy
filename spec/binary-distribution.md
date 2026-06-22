# Binary backend & distribution

**Status: shipped.** Codegen emits a wasm **binary** directly through the WIR
pipeline (`codegen::compile_module_binary` → `wir_encode`); the WAT text format
and the `wat` crate are gone. The interpreter is an oracle island off the
production path. This document records the design and the work that delivered it.

The follow-on to `rfcs/oracle-only-migration.md` (which made the compiled WASM
backend the one execution path and demoted the interpreter to an oracle). It
made witchy a **lean, single, binary-emitting compiler** and gave it a
**distribution story**: a compiled program is a portable `.wasm` module that a
trusted `witchy` host instantiates under a delegated capability grant. It was
three work items — **B** (slim), **A** (binary emission / cut WAT), **C**
(distribution) — sequenced B → A → C.

> **Note — `build-exe` removed.** An earlier Tier-2 packaged the program *plus* an
> embedded runtime into one self-contained executable (`witchy build-exe`). It was
> removed because it collapses the trust boundary: the runtime that is supposed to
> sandbox untrusted code would be supplied by the (untrusted) program's author, who
> could ship one that ignores the capability flags. The capability model only holds
> when the runtime is the **consumer's** trusted `witchy`. Distribution is therefore:
> ship the `.wasm`, and the consumer runs it with their own `witchy`.

## Goal (definition of done)

- **No WAT in the tree.** Codegen emits a wasm **binary** directly; the WAT text
  format and the `wat` crate are gone. (Disassembly, if ever needed, is an
  external tool over the binary — not a maintained second emitter.)
- **The interpreter is a feature-gated oracle island.** Nothing on the production
  or distribution path depends on its evaluator; `native` no longer depends on
  `interpreter`. The wasm32 build can exclude the evaluator entirely.
- **A program is distributable.** `witchy app.wasm` runs a portable module under
  the consumer's trusted `witchy`; the browser runs the *same* module via the JS
  host.
- **One artifact, two hosts.** A compiled program is a `.wasm` module plus its
  capability-import contract and source-derived footprint. A "host" is a
  per-environment implementation of that contract:

  | Host | Provides the `witchy.*` imports + enforces grants |
  |---|---|
  | native `witchy` / pm / build | wasmtime + `runtime.rs` |
  | browser | JS — `web/witchy-host.js` (no wasmtime in a browser) |

- **Every security invariant from the oracle-only migration is preserved** (see
  Invariants).

## End state

```
codegen:  AST → wasm IR → binary            (no WAT, anywhere)
artifact: app.wasm   (witchy host imports + source-derived footprint)

run it:   witchy app.wasm --dir . --net api:443         host = consumer's witchy
browser:  app.wasm + web/witchy-host.js (loader)        host = JS

                          source.witchy
                               │
            FRONT-END (lexer → parser → linker → typeck → lowering)  [shared]
                               │  lowered AST
          ┌────────────────────┼─────────────────────────┐
          ▼                    ▼                          ▼
  capabilities::analyze   CODEGEN (A)                INTERPRETER (B)
  footprint from source   AST → WIR → binary         [oracle + comptime: not a run path]
  → grant decision        (wasm-encoder)             tree-walks the AST:
                          │                          parity ·
                          ▼                          effectful build · demo
                     app.wasm ──────────► two hosts (native / browser)
                                                 │
                       SHARED CORE (always compiled): confine.rs · fmt.rs · value/native
```

## Invariants (non-negotiable — carry over from the oracle-only migration)

- **Two independent semantics + the parity gate stay.** The interpreter oracle is
  the independent check on the hand-written codegen; never replace it with running
  the same wasm on a second engine.
- **One confinement implementation, shared.** `resolve`/`resolve_write` (the
  `Dir`/`Net` escape checks) live in **one** module (`confine.rs` after B-1),
  imported by the native sandbox AND the interpreter — never a second copy.
  Feature-gating the evaluator must NOT gate out `confine.rs`. A capability-
  granting **browser** host delegates path checks to a `confine.rs` lib export
  (the same trick the playground already uses for `regex`/`crypto`), so it does
  not fork the check either.
- **Authority is source-derived and host-mediated, independent of emission.** A
  program's authority is its host-import set + the footprint from
  `capabilities::analyze` (the AST) — identical whether codegen emits WAT (pre-A)
  or the binary it emits now. wasmtime validates a binary module exactly as it did WAT.
- **The `Secret` seed never enters guest memory.** Enforced in the runtime host
  (it reads the seed from `caps.signing_key`, not guest memory), so B-3's
  `NativeValue::Secret([u8;32])` preserves it.
- **No silent capability grants.** A host links only the host functions the
  footprint declares; concrete resources (`--dir`, `--net`, `--signing-key`) are
  granted at launch.

## Work item B — interpreter slim (do FIRST: low risk, fixes a real coupling)

Decouple the pieces the compiled/native/browser paths borrow so the interpreter's
~3.7k-line evaluator can be feature-gated out, and fix the upside-down dependency
(`native` currently depends on `interpreter::Value`). Each step lands parity-green.

Current borrowed surface (in `src/interpreter.rs`): `Value` (enum, ~L27),
`render_float` (~L102), `RuntimeError` (~L175), `resolve`/`resolve_write`
(~L2204/L2232). The runtime sandbox reuses `resolve` via `confine(...)` in
`runtime.rs` (~L910–933). `native.rs` uses only `Value::{Str, Int, Bool, List,
Secret}`. `lib.rs` uses `render_float` + `Value`.

1. **`confine.rs`** — move `resolve`/`resolve_write`/`RuntimeError` there; re-point
   `runtime.rs` and `interpreter.rs`. Pure move. Acceptance: `Dir` escape tests
   green on both backends.
2. **`fmt.rs`** — move `render_float` there; re-point `lib.rs` and the
   interpreter's `Display`. Trivial.
3. **`NativeValue`** — define a minimal value type `{ Str, Int, Bool, List, Secret }`
   (e.g. `value::NativeValue`); switch the `native` registry signature, the
   `runtime.rs` host bridges, and `lib.rs` to it. The interpreter converts to/from
   it only at its own native-call sites. Acceptance: `native` no longer references
   `interpreter::*`; crypto/regex/parity tests green.
4. **Feature-gate the evaluator** (`run_module*`, `run_build_step`, `eval`) — and
   **measure the bundle delta first**. **Finding (done): not feasible / not
   justified.** After B-1..3, the *only* always-compiled reference to the
   interpreter is `comptime.rs`, which evaluates `comptime` blocks at compile time
   via `interpreter::run_module_budgeted` (called from `linker::link`, in the
   compile path the wasm playground uses too). So the evaluator is a **compile-time
   dependency present in every build that compiles witchy**, not a strippable
   oracle-only component. The CLI/oracle-only entries (`run_module_exit`,
   `run_with`, `run_program`, `run_build_step`) are already unreferenced from the
   wasm lib and stripped by wasm-opt. Gating the *core* evaluator out would break
   comptime; the real prerequisite is **decoupling comptime** (give it its own
   compile-time const-evaluator, or run comptime on the compiled backend) — a
   separate, larger effort. Deferred until then.

   This sharpens the interpreter's role: it is the oracle, the runtime fallback /
   effectful-build executor / demo, **and** the comptime const-evaluator (a
   genuine compiler component). "Drop it from the bundle entirely" was never fully
   reachable while comptime exists; B-1..3's decoupling (clean boundaries, `native`
   no longer depending on `interpreter`) is the achievable and delivered win.

Effort: medium (mostly mechanical moves). Risk: low–medium. Outcome: B-1..3
shipped; B-4 measured and deferred (blocked by comptime).

## Work item A — binary emission via a typed wasm IR (cut WAT)

*(Delivered: codegen now lowers to WIR and `codegen::compile_module_binary` →
`wir_encode::encode` returns a `Vec<u8>` binary. The pre-migration state and the
plan that got us here are recorded below.)*

Before this work, `codegen::compile_module` returned a `String` of WAT (~695
`push_str`/`format!` emission sites across ~9.3k lines); wasmtime parsed that WAT
and the browser assembled it with the `wat` crate. The plan replaced text
emission with a binary via `wasm-encoder`.

Do it through an **IR in the middle**, never a direct string→encoder sweep:

1. Define a small typed instruction/section IR.
2. Convert the ~695 emission sites to emit IR nodes (mechanical, reviewable).
3. Write one IR→binary backend over `wasm-encoder` (sections: types, imports,
   funcs, code, exports, memory, data, globals, elements).
4. **Migration safety net:** keep the old WAT path alongside temporarily and assert
   the new binary *runs* identically to the old WAT across the example corpus +
   parity + the existing metamorphic checks. A throwaway IR→WAT printer can back a
   text diff during migration — it is scaffolding, **deleted at the end**, not a
   feature.
5. Flip and cut: `compile_module -> Vec<u8>`; drop `wat::parse_str` in
   `lib::compile_source` (browser gets the binary directly); pass binary to
   `Module::new` / `optimize_module`; **remove the `wat` dependency**; drop
   `witchy emit-wat` (or re-implement as a one-line shell-out to an external
   disassembler).

Acceptance: no hand-written WAT emitter in the tree; `wat` crate gone; parity +
metamorphic green; the binary runs identically to pre-A across the corpus; the
browser path no longer assembles WAT. Effort: large. Risk: high but mitigated by
step 4's behavioral diff.

## Work item C — distribution + the hosts (DONE)

The compile artifact is **`app.wasm`**: the program module, importing the witchy
capability host. Authority is derived from its **imports** (a module can't call a
host op it doesn't import), the distribution counterpart of `capabilities::analyze`
on source (`witchy_imports` in `main.rs`).

- **Tier 1 — portable module (DONE).** `witchy emit-wasm app.witchy [-o app.wasm]`
  produces the binary; `witchy app.wasm` (or `witchy sandbox app.wasm --dir . --net
  api:443`) runs it. The installed `witchy` binary is the host: it reads the
  module's `witchy.*` imports, grants exactly those families (Dir/Net at the
  `--dir`/`--net` roots, default cwd; `Secret` requires `--signing-key`), and a
  module importing an ungranted op fails to instantiate. Validated by
  `precompiled_wasm_runs_like_the_source`.
- **Browser host (already shipped in the oracle-only migration).** Ship `app.wasm`
  + `web/witchy-host.js` as the loader. Default authority is none (every
  `Dir`/`Net`/`Clock` import traps). A richer host that grants capabilities
  implements them against browser APIs and delegates confinement to a `confine.rs`
  lib export (per the Invariants).

The capability guarantee travels with the artifact in every tier: the host links
only the imported families, grants are launch-time, and `Secret`/confinement go
through the same `runtime.rs`/`confine.rs` as `witchy sandbox`. A "binary" does not
trade away the security model — it is `witchy sandbox` frozen.

Future polish (not blockers): embed a per-target `cwasm` + a slim runtime-only
launcher; embed the source footprint as a custom section so `witchy caps app.wasm`
reads it directly rather than re-deriving from imports.

## Sequencing & status

```
B (slim) ✅ DONE ──► C (distribution) ✅ DONE      A (binary emission) — DEFERRED
   confine/fmt/value      emit-wasm · run .wasm        does NOT reduce the
   native ↛ interpreter   (consumer's witchy)          backend/mode count
```

- **B ✅** — `confine.rs`/`fmt.rs`/`value.rs` extracted; `native` no longer depends
  on `interpreter`; B-4 measured & deferred (the evaluator is a comptime
  compile-time dependency, not strippable). All parity-green.
- **C ✅** — `emit-wasm` and running a precompiled `.wasm` (authority from its
  imports), tested. The distribution artifact is a wasm **binary** emitted directly
  via `wir_encode`, run by the consumer's trusted `witchy`.

### Direction change: minimize "ways", and A is deferred

The owner's call (mid-build): **minimize how many backends/modes the compiler has**;
do what's *fastest* to the simple end state. That reframes — and ultimately
**cancels** — A.

The `wat` crate has since been removed entirely: codegen lowers the checked AST
to a structured IR (`WirModule`) and `src/wir_encode.rs` emits the wasm **binary**
directly via `wasm-encoder` — there is no WAT-text assembly in the run path, and
`wat` is no longer a direct dependency (it survives only transitively under
wasmtime).

What a parallel IR backend *did* add was a **third way the compiler emits code**,
which is the opposite of the goal — so it was removed:

- **Removed** `src/wasm_ir.rs` and `src/codegen_ir.rs` (the parallel IR codegen
  experiment). `wasm-encoder` was kept and is now the one binary emitter.
- **Removed** the `WITCHY_INTERP` run-fallback mode — `witchy run` is the compiled
  backend, full stop; the interpreter is only the oracle + comptime evaluator.
- **Deferred:** removing the `witchy demo` showcase (~200 lines of the original
  language spike + its helpers). It's a pre-existing minor command,
  not new proliferation — a low-value, mechanical cleanup left for a focused pass.

End state of "fewer ways": **one front-end, one compiled backend (AST → WIR →
wasm binary via `wasm-encoder`), one interpreter used only as the parity oracle
+ comptime evaluator.**

## Out of scope (separate, additive future work)

- **WASI retarget** — mapping the effect imports onto WASI preview 2
  (`wasi:filesystem`/`sockets`/`clocks`) so a witchy `.wasm` runs on *generic*
  runtimes (wasmtime CLI, jco, wasmCloud) with no witchy host. Witchy's `Dir`/`Net`
  grants map onto WASI's own capability model. Buys ecosystem portability; the
  distribution story above needs none of it.
