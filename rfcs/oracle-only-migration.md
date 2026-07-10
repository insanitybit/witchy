---
status: implemented
note: Imported from docs/ under RFC-0001. Frozen design record — current behavior lives in spec/ and the code.
---

# Migration: demote the interpreter to a dev-only oracle

## Status

**Complete.** The compiled WASM backend is the sole user-program run path; the
interpreter is a dev-only oracle (parity), the `comptime` evaluator, the test
runner, the effectful-build executor, and `witchy demo`. There is **no
interpreter fallback** on the run path — a program that does not compile is a
hard error — and the `WITCHY_INTERP` escape hatch was removed (a stray mention
survives only in `interpreter.rs`'s module comment). The planning sections
further down are kept as the historical record; some of their identifiers
(`codegen::compile_module`, the `wat` crate) predate later refactors — codegen
now lowers to WIR and `compile_module_binary` → `wir_encode` emits the binary
directly.

The migration shipped across these phases (full suite green, plus a no-oracle
metamorphic check):

- **Phase 0–1 — DONE.** `witchy run` (`execute_file_exit`) executes on the
  compiled backend via the shared `run_linked_compiled` helper (footprint-derived
  grants, `Dir` rooted at cwd), with the validated module caches making re-runs fast
  (cold/warm ≈ 19 ms). On the run path the compiled backend is used
  unconditionally — there is no interpreter fallback. A `Secret` is
  required only when `main` actually binds one (matching the interpreter), not
  whenever the whole-program footprint mentions signing.
- **Phase 2 — DONE (with a principled boundary).** Deterministic build steps
  (only `BuildOut`/`BuildRead`) run in the zero-ambient WASM sandbox in both the
  `witchy build` CLI and the package manager. Effectful steps
  (`BuildExec`/`BuildNet`/`BuildEnv`) stay on the capability-sound interpreter on
  purpose: their confinement is the host-side allow-list, which the WASM boundary
  cannot itself enforce, so moving them yields zero isolation benefit.
- **Phase 3 — DONE.** The self-hosted `coven`/`pm` modules all compile to WASM, so
  running them through `execute_file_exit` already uses the compiled backend. The
  remaining `interpreter::run_program`/`run_with` callers are demos and the oracle
  side of differential tests.
- **Phase 5 — effectively satisfied for "does it compile".** 105 of 106 shipped
  examples compile to WASM (the one that does not is a library with no `main`).
  The "interpreter-only feature" surface (actors, networking, float formatting) is
  gone, so no interpreter-only feature blocks compilation. What remains of Phase 5 is the
  *deduplication* refactor (push intrinsics into `std` so the oracle is cheap to
  maintain) — see the phase below.
- **Phase 4 — DONE.** The playground compiles each snippet to a wasm binary with
  the compiler-as-wasm (`lib::compile_source`, which lowers to WIR and encodes a
  wasm binary directly — no `wat` crate) and runs THAT module on the browser's
  own engine. The
  page is the capability host: `print` collects output; the pure helpers
  (`float_to_str`, `string_from_code`, `encoding`) delegate to lib exports
  (`witchy_render_float` / `witchy_string_from_code` / `witchy_encoding`) so they
  match the native backend exactly; every authority import (Dir/Net/Clock/Env/
  Secret) is a trapping stub — the browser grants nothing. The interpreter no
  longer runs user code in the browser. Validated two ways: a native differential
  test that the assembled binary runs identically to the oracle for every console
  example, and a Node harness
  (`scripts/pg_validate.mjs`, V8 like Chrome) running the *actual* `web/witchy.wasm`
  — **94 programs byte-identical to the interpreter** (including the `regex`
  example, via the staged helper below); the only mismatches are Dir/Net programs
  correctly trapping (the browser grants no capabilities).
- **Phase 7 — STARTED.** Added `examples_agree_under_inplace_and_forced_copy`: a
  corpus-wide metamorphic check that the in-place and forced-copy lowerings agree,
  with no interpreter reference. Docs (`architecture.md`, this file) updated.

- **Phase 6 — DEMOTED.** The interpreter is no longer on any default
  user-program path; its module doc now declares its oracle role. The only
  remaining `interpreter::*` runtime callers are the oracle, the effectful-build
  executor, and `witchy demo` — each a documented, deliberate retention.
  Shrinking the file itself is blocked by the sandbox sharing its confinement
  logic (a separate extraction).

All seven phases are implemented or demonstrably satisfied; the full suite is
green and the browser path is validated against the oracle (94 programs
byte-identical, including `regex`/`crypto`). Two **future refactors** remain —
improvements, not gaps in the migration's goal:

1. Emit wasm **binary directly** via `wasm-encoder`, retiring the WAT-assembly
   step and shrinking both the codegen bug surface and the playground bundle.
2. The deeper interpreter **slim**: extract the shared `Dir`/`Net` confinement and
   the `render_float`/`Value` helpers the codegen/native paths borrow, then reduce
   the evaluator to the primitive core so the wasm bundle can drop it entirely.

## Goal (definition of done)

`src/interpreter.rs` is reachable **only** through the differential test harness
and `witchy parity` (the `*_backends_agree` tests, the proptest fuzzer, the
`WITCHY_NO_INPLACE` forced-copy diff). Every path a *user* can trigger —
`witchy run <file>`, build-step execution, the `coven` package manager, and the
browser playground — goes through `codegen → WASM → wasmtime` (or the browser's
WASM engine). The interpreter becomes a test tool for people hacking on witchy
itself, never something an end user's program runs on.

This is a consolidation, not a deletion: one production runtime, one small
reference evaluator that only CI invokes.

## The sandbox keeps working — it becomes the foundation

`witchy sandbox` already *is* the `codegen::compile_module → runtime::spawn`
path with a `runtime::Capabilities` grant set and the zero-ambient wasmtime host
in `src/runtime.rs`. Nothing in this plan weakens or removes it; every other
execution path is rerouted *onto* it. The security boundary (capability host
functions, no ambient authority, footprint computed from source) is unchanged.

## What runs on the interpreter today (the things to re-home)

Confirmed call sites in `src/main.rs`:

| Role | Entry point | Current grant shape |
|---|---|---|
| `witchy run <file>` (default dev run) | `execute_file_exit` → `interpreter::run_module_exit` (main.rs:1085) | `Dir@cwd`, `net_allow`, `args`, `signing_key` |
| Build-step execution | `run_build_step` (main.rs:1469) | `interpreter::BuildGrants` |
| Package manager / `coven` | `run_program` (1580), `run_with` (1621, 1636) | program sources + entry |
| Differential oracle | `*_backends_agree` tests, `witchy parity`, proptest | both backends |

The compiled side already exists alongside each: `codegen::compile_module`
(main.rs:799, 1207, 1266, …), `codegen::compile_build_module` (1381), and
`runtime::spawn` with `runtime::Capabilities`.

## The crux: capability mapping

The single hard dependency for the whole migration is making
`runtime::Capabilities` + the wasmtime host express the **full** grant set the
interpreter run paths hand out today, not just `{print, print_int}`:

- **`Dir@cwd`** — a directory capability rooted at the current directory, with
  the same path-confinement the interpreter enforces (`run_in` / escape
  rejection tests at main.rs:3883–3909 are the spec to match).
- **Net allowlist** (`net_allow`) — the `--net host:port` grants, enforced at
  the host boundary.
- **`args`** — the `List(String)` argv parameter (`run_module_args`).
- **Root `Secret`** (`signing_key`) — the signing-key capability
  (`run_module_signed`).

Acceptance for this crux: a compiled-backend `execute` that takes the same
`(net_allow, args, signing_key)` and a `Dir@cwd`, and passes every test that
currently asserts interpreter confinement behavior, on the WASM backend.

## Phases

Each phase is independently shippable and leaves the tree green. Parity stays on
until the very end, so every reroute is validated against the oracle as it lands.

### Phase 0 — Measure and gate

- Benchmark cold-start of the compiled path for a trivial program: front-end
  (parse/link/typeck/footprint) + `Engine::new` + `spawn`. Establish the dev-run
  latency floor with the optimized-wasm and Wasmtime compilation caches warm
  and cold (`build_module` / `Module::new`).
- Decide the acceptable dev-run latency budget. If the warm-cache number is
  within budget, Phase 1 is unblocked; if not, cache/Engine-reuse work comes first.
- Inventory current parity coverage so we know what the oracle actually guards
  before we lean on it harder.

### Phase 1 — `witchy run` on WASM

- Implement the capability-mapping crux above: extend `runtime::Capabilities`
  and the host so a compiled run can be granted `Dir@cwd` + net allowlist +
  argv + root `Secret`.
- Reroute `execute_file_exit` (main.rs:1085) from `interpreter::run_module_exit`
  to `codegen::compile_module` → `runtime::spawn` with that grant set, returning
  the same `(Vec<String>, exit_code)`.
- Wire the validated module caches so re-running an unchanged file skips
  Binaryen and native recompilation.
- Keep an **undocumented** `WITCHY_INTERP=1` escape hatch that forces the
  interpreter path — for bisecting backend disagreements during development
  only, not a user feature.
- Acceptance: the entire example corpus produces byte-identical output through
  `witchy run` before and after the reroute; confinement/argv/secret tests pass
  on the compiled path.

### Phase 2 — Build steps in the WASM sandbox

- Reroute `run_build_step` (main.rs:1469) onto `codegen::compile_build_module`
  (already present at main.rs:1381) + `runtime::spawn`, translating
  `interpreter::BuildGrants` into `runtime::Capabilities`.
- This is a **security upgrade**: build steps run untrusted dependency code, and
  the zero-ambient sandbox is strictly stronger than the interpreter for that.
  Deterministic steps especially belong here (reproducibility).
- Acceptance: every example project's build (examples/projects/*) produces
  identical artifacts; declared-footprint enforcement still rejects over-reach.

### Phase 3 — Package manager / `coven` on WASM

- Reroute `run_program` (1580) and `run_with` (1621/1636) — the self-hosted pm
  and coven registry — onto the compiled path.
- `coven` is itself a witchy program; running it through codegen is also the
  best large, real-world soak test of the compiled backend.
- Acceptance: pm client + coven HTTP registry round-trip (signed records verify)
  exactly as today, now compiled.

### Phase 4 — Browser playground on the codegen path (DONE)

The trick that avoided a byte-exact JS reimplementation of the pure host
functions: the lib **exports** them, so the page delegates to the real Rust code
instead of reproducing it. Float formatting and hex/base64 therefore match the
native backend by construction, not by careful imitation.

What shipped:

1. **`lib::compile_source(src) -> Result<Vec<u8>, String>`** — resolve against the
   bundled std, type-check, `codegen::compile_module` to WAT, assemble to a wasm
   binary with the pure-Rust `wat` crate. The wasm32 ABI export `witchy_compile`
   returns `[u32 status][u32 len][payload]`.
2. **Lib helper exports** `witchy_render_float` / `witchy_string_from_code` /
   `witchy_encoding`, each reusing the shared `interpreter::render_float` /
   `native::*` logic both backends already use.
3. **`web/playground.js`** now compiles the snippet, instantiates the resulting
   module on the browser's `WebAssembly` engine, and provides the `witchy.*`
   imports: `print`/`print_int`/`print_float` collect output; the pure helpers
   delegate to (2) — including `regex.match_spans` (staged through
   `regex_match_spans_len` + `fill_pending`, like the native runtime) and the pure
   `crypto.*` hashes/verifies (`witchy_crypto_hash` / `witchy_hmac_sha256` /
   `witchy_verify`); a `Proxy` makes every authority import (Dir/Net/Clock/Env/
   Secret) a trapping stub, since the browser grants no capabilities. Delegating
   `regex`/`crypto` matters because the browser has no filesystem, so `import regex`
   resolves to the *bundled* (native-backed) std module, not a pure sibling.
4. **Validation** — `assembled_binary_runs_like_the_wat` (native test: the
   assembled binary runs identically to the WAT for every console example) and
   `scripts/pg_validate.mjs` (a Node/V8 harness that runs the actual
   `web/witchy.wasm`): 94 programs byte-identical to the interpreter, including the
   `regex` example.

- Acceptance: ✅ the playground runs the example corpus with output matching
  `witchy run`; the interpreter no longer runs user code in the browser.
- Longer-term codegen-trust improvement (not a gap): emit wasm **binary directly**
  via `wasm-encoder`, retiring the WAT-assembly step and shrinking the bundle.
- Note on "no interpreter in the bundle": the interpreter's *evaluator* is no
  longer reachable from the wasm entry points (so wasm-opt can strip it), but a
  few non-eval items it still owns — `render_float`, the `Value` type the native
  registry uses — keep parts of the module compiled in until they're factored out
  (a Phase-6 extraction).

### Phase 5 — Shrink the duplicated primitive surface (make the oracle cheap)

This is the lever that makes keeping the oracle low-cost rather than maintaining
a second full runtime.

**Audit result (DONE).** The intrinsic surface is *already* minimal — the
"stdlib, not builtins" direction drove it down long ago. The interpreter's entire
builtin dispatch is just the irreducible primitive core below; `list`, `string`,
`dict`, `math`, `set`, `option`, `result`, `iter`, `json`, `time`, … are all
witchy `std` modules that run identically on both backends with **zero**
backend-specific code. So there is nothing left to "push into std".

The **true primitive core** (implemented per-backend by necessity — the cost of
the oracle):

- **Capability host ops** (authority-bearing, host-mediated, cannot be library
  code):
  - Console — `print`
  - Dir — `subdir`, `read`, `write`, `append`, `exists`, `is_dir`, `list`, `make_dir`
  - Clock — `now`; Env — `get_env`
  - Net — `restrict`, `connect`, `try_connect`, `send_line`, `recv_line`,
    `send_bytes`, `recv_all`, `recv_bytes`, `listen`, `accept`, `close`
  - Build — `write_out`, `read_build`, `get_build_env`, `fetch_build`, `run_tool`
  - Secret — host-side `sign` / `public_key`
- **Language primitives** — `__render` (type-directed `Display`), `fail` (abort/
  trap), `int_to_duration` / `duration_to_int`, plus the value model the codegen
  emits inline and the interpreter evaluates: arithmetic/comparison/equality
  (type-directed), indexing, allocation.
- **Pure native helpers** — `float_to_str`, `string_from_code`, `crypto.*`,
  `regex`, `encoding`. These are **not** duplicated: they live once in
  `src/native.rs`; the interpreter calls the registry directly and the compiled
  backend reaches the same registry through a host import (`runtime.rs`).

- Acceptance: the intrinsic set is documented and minimized; std-implemented ops
  carry no backend-specific code. ✅ Met — the list above *is* the set, and it is
  the floor, not a backlog.

### Phase 6 — Demote and slim the interpreter (DEMOTED; full slim constrained)

**Done:** the interpreter is demoted — it no longer runs user programs on the
default path. `interpreter.rs`'s module doc now declares its oracle role, and
`witchy run` / `sandbox` / pm / the browser all use the compiled backend.

The remaining non-test `interpreter::*` runtime callers are **deliberate, not an
oversight**, and each is documented at its call site:

1. **`witchy parity` + the differential tests** — the oracle itself (the point).
2. **The `comptime` evaluator** — `comptime:` blocks run on the interpreter at
   compile time (deterministic, zero capabilities).
3. **Effectful build steps** (BuildExec/BuildNet/BuildEnv) — the capability-sound
   executor; moving them to WASM yields no isolation benefit (the allow-list is
   the confinement) and would duplicate host functions.
4. **`witchy demo`** — a self-contained capability/runtime showcase.

**Constraint on "materially smaller file":** the interpreter is a *live* reference
evaluator, and its `Dir`/`Net` path-confinement (`resolve`) is reused by the
sandbox host. So it cannot simply shrink to the primitive core while it is still
the oracle and the confinement source. A genuine slim would first extract the
shared confinement and the `render_float` / `Value` helpers the codegen/native
paths borrow — a separate refactor tracked as future work, not a blocker for the
migration's goal (the interpreter is already not user-facing for execution).

- Acceptance (revised to reality): the interpreter has zero callers that run an
  end-user program on the *default* path; every remaining caller is the oracle,
  the `comptime` evaluator, the effectful-build executor, or the demo. ✅

### Phase 7 — Re-found parity as a CI concern + add no-reference checks (DONE)

- **Parity is now a test gate, not a product guarantee.** Because `witchy run`
  and `witchy sandbox` are the same backend, dev == deploy by construction;
  parity's remaining job is to catch codegen bugs in CI. `architecture.md` now
  says so.
- **No-oracle self-checks** (defense in depth beyond the reference evaluator):
  - **Added** `examples_agree_under_inplace_and_forced_copy` — corpus-wide, the
    in-place and forced-copy lowerings must produce identical output, no
    interpreter involved.
  - **Added** `assembled_binary_runs_like_the_wat` — the `wat`-assembled binary
    (the browser path) runs identically to the WAT text, corpus-wide.
  - Already present: the `WITCHY_NO_INPLACE` forced-copy differential and `fmt`
    round-trip / reformat-idempotence.
  - Same family, easy follow-ups: opt-level agreement and optimized-wasm cache
    hit vs freshly-optimized agreement (toggle the optimizer/cache and diff output).
- Acceptance: ✅ CI runs `parity` over every example plus the metamorphic checks
  above; a codegen regression is caught by at least one without needing a
  reference implementation.

## Non-negotiables / risks

- **The oracle must stay an *independent* implementation.** Do not replace it
  with "run the same WASM on a second engine" — that catches engine bugs, not
  *our* codegen bugs (both would run codegen's output). The reference evaluator
  earns its keep precisely by not sharing the codegen path.
- **Cold-start is the main risk** to `witchy run` ergonomics. If the warm-cache
  number misses budget, fix it in Phase 0 before rerouting; do not ship a slow
  dev loop.
- **Footprint must remain source-derived.** The security model recomputes the
  capability footprint from source per run, so the front-end can't be cached
  away even when the compiled module is.
- **Confinement parity is the acceptance bar for Phase 1**, not output parity
  alone — the `Dir` escape-rejection and net-allowlist tests are the spec.
- **One confinement implementation, shared by both backends.** The WASM sandbox
  enforces `Dir` escapes via the *same* `resolve`/`resolve_write` the interpreter
  uses (`runtime.rs` `confine(...)`). Refactor B relocates that code to `confine.rs`
  but must keep it a single source imported by both — never a second copy in the
  runtime, or the two backends can drift on what "escape" means. The both-backend
  escape tests guard this. Feature-gating the evaluator (B-4) must NOT gate out
  `confine.rs`: the native sandbox needs it even with the evaluator excluded.
- **Authority stays source-derived and host-mediated, independent of emission.**
  A program's authority is its host-import set + the footprint computed from the
  AST — unchanged whether codegen emits WAT (A: today) or binary (A: future), and
  the `Secret` seed never enters guest memory (enforced in the runtime host, not
  the value type, so B-3's `NativeValue::Secret` preserves it). If A lands, keep a
  WAT/disassembly view so `witchy emit-wat` can still audit the module the sandbox
  runs.

## Future refactors — detailed scope

Two improvements remain past the seven phases. Neither is required for the
migration's goal (the interpreter is already not the execution path); both are
scoped here so they can be picked up deliberately.

### A. Direct wasm-binary emission (`wasm-encoder`)

**What it is.** `codegen::compile_module` returns a `String` of WAT text (~695
`push_str`/`format!` emission sites across ~9.3k lines); wasmtime parses that WAT
(`Module::new` takes WAT or binary), and the browser assembles it with the `wat`
crate. This refactor makes codegen emit a wasm **binary** directly via
`wasm-encoder` (already in the dep tree through `wat`).

**Honest value.** Modest. The three claimed wins are weak in practice:
- *Bug surface* — structured emission is harder to get wrong than string WAT, but
  the WAT path is already covered by parity + `assembled_binary_runs_like_the_wat`
  + `examples_agree_under_inplace_and_forced_copy` + 1175 tests.
- *Drop the WAT step* — the browser's `wat` assembly and wasmtime's WAT parse both
  work fine; removing them is tidiness, not a fix.
- *Speed* — WAT parsing is a fraction of the ~16 ms cold-start floor and is
  cached as validated optimized wasm after the first run.
The real drivers would be **new needs**: source maps / DWARF, wasm GC or other
post-MVP features, or a measured compile-speed bottleneck.

**Scope.** A near-total rewrite of the emission layer. The control-flow, locals,
and value-stack logic stay; only the output sink changes. The lower-risk path is
an **IR in the middle**, not a big-bang rewrite:
1. Introduce a small typed instruction IR; change the ~695 sites from emitting
   strings to emitting IR nodes (mechanical, reviewable).
2. Write one IR→binary backend over `wasm-encoder` (sections: types, imports,
   funcs, code, exports, memory, data, globals).
3. Keep an IR→WAT printer during migration and assert it reproduces the old WAT
   byte-for-byte (a built-in differential safety net), then retire it.
4. Update call sites: `compile_module -> Vec<u8>`, drop `wat::parse_str` in
   `lib::compile_source`, pass binary straight to `Module::new` / `optimize_module`.

**Effort:** large (multi-day). **Risk:** high but mitigatable (the IR→WAT diff +
parity catch regressions). **Recommendation: defer** until a concrete driver
appears; if taken on, do it via the IR path, never a direct string→encoder sweep.

### B. Deep interpreter slim (decouple the shared bits, gate out the evaluator)

**What it is.** The interpreter can't be deleted — it's the oracle, the
`comptime` evaluator, the effectful-build executor, and `witchy demo`. The
goal is to **decouple** the pieces the compiled/native/browser paths borrow so the
interpreter's ~3.7k-line evaluator can be feature-gated out of the wasm build and
the module boundaries stop being upside-down (today `native` depends on
`interpreter::Value`).

**The borrowed surface (what to extract):**
- `resolve` / `resolve_write` / `RuntimeError` (interpreter.rs) — the Dir/Net
  confinement the **runtime sandbox reuses** (`runtime.rs` `confine(...)`).
- `render_float` — used by `lib::compile_source`'s host helpers.
- `Value` — but `native.rs` only uses five variants (`Str`/`Int`/`Bool`/`List`/
  `Secret`), so it needs a *minimal* value type, not the interpreter's full enum.

**Scope (sequenced, each independently shippable + parity-green):**
1. Move `resolve`/`resolve_write`/`RuntimeError` to a new `src/confine.rs`; re-point
   `runtime.rs` and `interpreter.rs`. Pure move, low risk.
2. Move `render_float` to a small `src/fmt.rs`; re-point `lib.rs` and the
   interpreter's `Display`. Trivial.
3. Define a minimal `value::NativeValue { Str, Int, Bool, List, Secret }`; switch
   the `native` registry, the `runtime.rs` host bridges, and `lib.rs` to it. The
   interpreter converts to/from it only at its own native-call sites. Medium effort
   (the native registry signature changes), covered by the crypto/regex/parity tests.
4. Feature-gate the evaluator (`run_module*`, `run_build_step`, `eval`) behind an
   `oracle` feature off in the wasm/`--no-default-features` build; keep
   confine/fmt/value always compiled. **Measure the bundle delta** — likely small,
   since wasm-opt already strips the unreferenced evaluator, so do step 4 only if
   the number justifies it.

**Effort:** medium (mostly mechanical moves). **Risk:** low–medium. **Value:**
mainly architectural hygiene (clean boundaries, `native` no longer depends on the
interpreter) plus a measure-first bundle win. **Recommendation: do steps 1–3**
regardless — they're cheap and fix a real coupling; gate (step 4) on the measured
size win.

**A vs B:** independent. **B is the better ROI** (lower risk, fixes a real
coupling); A is deferrable until a concrete driver shows up.

## Sequencing

```
Phase 0 (measure) ──► Phase 1 (run on WASM) ──► Phase 2 (build) ──► Phase 3 (pm)
                                   │
                                   └─► Phase 4 (browser) needs binary emission ─┐
Phase 5 (shrink intrinsics) ───────────────────────────────────────────────────┤
                                                                                ▼
                                              Phase 6 (demote interpreter) ──► Phase 7 (CI parity)
```

Phases 1–3 are the load-bearing reroutes and can land in order. Phase 4
(browser) and Phase 5 (shrink intrinsics) are parallelizable against them.
Phase 6 can only complete once 1–4 have removed the last user-facing caller.
Binary emission (`wasm-encoder`) is the shared prerequisite that unblocks both
the browser and the codegen-trust story.
