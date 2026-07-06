---
rfc: 0068
title: "Fine-grained exec/env grants on the compiled backend (retire the build-step interpreter holdout)"
status: proposed
created: 2026-07-06
related:
  - "0001 (the parity prime directive: interpreter = differential-testing oracle only)"
  - "0004 (self-hosted CLI — pm/coven now run compiled; this closes the remaining holdout)"
  - "0011/0012/0013 (capability refinement — this is the compiled-tier half of build-cap enforcement)"
tracking:
---

# RFC-0068: Fine-grained exec/env grants on the compiled backend

> Provisional throughout. Code blocks are intentionally **not** tagged `witchy` so the
> doc-examples sweep does not compile pre-implementation snippets.

## Summary

The interpreter is meant to be the **differential-testing oracle only**; the compiled
WASM tier is the sole production run path. Two recent fixes removed the last two
production paths that still ran through the tree-walker: `witchy pm`/`coven-serve`
(compiled) and `witchy test` (compiled). **One deliberate holdout remains:** a `build`
step that needs `BuildExec`, `BuildNet`, or `BuildEnv` runs on the interpreter, because
the compiled backend cannot express the *fine-grained allow-lists* those grants carry.

This RFC closes that gap: teach the compiled backend to enforce **per-tool exec** and
**per-key env** allow-lists (net already has one), so every `build` step runs compiled
and the "interpreter = oracle only" invariant holds with no exceptions.

## Motivation

`run_build_step_file` (`src/main.rs`) already splits build steps by grant shape:

    // Deterministic steps (only BuildOut/BuildRead, no env/exec/net) are a pure
    // function of their inputs, so they run in the zero-ambient WASM sandbox where a
    // `..` write traps with no host import to call. Steps needing BuildExec/BuildNet/
    // BuildEnv run on the capability-sound interpreter: their host process/socket/env
    // I/O is confined by the grant allow-list, which the WASM boundary cannot itself
    // enforce.
    let sandboxable = env_keys.is_empty()
        && exec_tools.is_empty()
        && !footprint.build.is_empty()
        && footprint.build.keys().all(|k| *k == "BuildOut" || *k == "BuildRead");
    if sandboxable {
        return run_build_step_sandboxed(linked, out, read_roots);   // COMPILED
    }
    // ... else interpreter::run_build_step(linked, grants)          // INTERPRETER

The comment's claim — "the grant allow-list … the WASM boundary cannot itself enforce" —
is **half true, and the half that's false is now proven false**. The general run path
*does* enforce a net allow-list on the compiled backend today (`Capabilities.net_allow:
Option<Vec<String>>`, used by `coven-serve` and `witchy pm`). What is genuinely missing
is the **exec and env** equivalents:

- `Capabilities.exec: bool` — all-or-nothing. `host_exec_run` does `Command::new(&prog)`
  with **no tool allow-list check**.
- `Capabilities.env: bool` — all-or-nothing. No per-key filter.

Meanwhile the interpreter's `BuildGrants` carry exactly those allow-lists:

    BuildCap::Exec(exec_tools: Vec<String>)   // only these programs may be spawned
    BuildCap::Env(env_keys:  Vec<String>)     // only these keys may be read
    BuildCap::Net(net_hosts: Vec<String>)     // only these hosts may be reached
    BuildCap::Read(read_roots) / BuildCap::Out(out_dir)   // already compiled-sandboxable

So a step declaring `Build[Exec{cc}]` cannot run compiled without either (a) *over*-granting
(flip `exec` to `true` → the step could spawn *anything*, destroying the confinement the
build model promises) or (b) *under*-granting (`exec = false` → the step traps). The only
faithful option today is the interpreter. That is why the holdout exists — not an oversight,
a real enforcement gap.

### Why it matters

With pm/coven/`witchy test` migrated, `run_build_step_file`'s exec/env/net branch is the
**last deliberate interpreter invocation in a production path**. Closing it means the
interpreter is invoked *only* for: the parity oracle (`parity_check`), compile-time
const-eval (`comptime`/tagged literals), and benchmarks/tests. That is the invariant fully
realized — and it removes a second, subtler risk: a build step's behavior today is only ever
observed on the interpreter, so a build-step-only divergence on the compiled backend would
never be caught.

## Design

Mirror the existing `net_allow` treatment for exec and env, and extend the compiled
build-cap routing (which already handles `BuildOut`/`BuildRead`) to `BuildExec`/`BuildEnv`/
`BuildNet`.

### 1. Capabilities carries the allow-lists

    // crates/witchy-runtime/src/runtime.rs
    pub struct Capabilities {
        // ...
        pub exec: bool,                        // retained: general Exec grant (all tools)
        pub exec_allow: Option<Vec<String>>,   // NEW: if Some, only these program names
        pub env: bool,                         // retained: general Env grant (all keys)
        pub env_allow: Option<Vec<String>>,    // NEW: if Some, only these keys
        pub net_allow: Option<Vec<String>>,    // already exists — the template
    }

Semantics match `net_allow`: `None` = governed by the coarse bool (unchanged behavior for
existing callers); `Some(list)` = grant is present **and** restricted to `list`. `Some(vec![])`
is a present-but-empty grant (every operation denied) — exactly as an empty `net_allow`
already behaves.

### 2. Host functions enforce host-side

The enforcement point already receives the thing to check — no ABI widening of the *call
site* is needed, only a guard in the host function:

    // host_exec_run: the guest passes the program name; check it before spawning.
    if let Some(allow) = &caps.exec_allow {
        if !allow.iter().any(|t| t == &prog) {
            return abort("exec: `{prog}` is not in the granted tool allow-list");
        }
    }

    // the env-read host import: the guest passes the key; filter it.
    if let Some(allow) = &caps.env_allow {
        if !allow.iter().any(|k| k == &key) { return /* absent */ nil; }
    }

This is the same shape as the net allow-list check already performed on `connect`. The
abort message must match the interpreter's for message parity (RFC-0045 root-cause routing).

### 3. Route the build caps + widen `sandboxable`

`run_build_step_sandboxed` today grants only `BuildOut`/`BuildRead`. Generalize it (or add a
compiled sibling) to mint `Build[Exec]`/`Build[Env]`/`Build[Net]` and set the three
allow-list fields from `BuildGrants`, then broaden the gate:

    // No branch on grant *kind* anymore — every build step runs compiled. The grants
    // (Out/Read/Exec/Env/Net allow-lists) are enforced host-side, and a `..` escape or
    // an ungranted tool/key/host aborts with no host import to satisfy it.
    return run_build_step_compiled(linked, out, read_roots, exec_tools, env_keys, net_hosts);

The deterministic (`BuildOut`/`BuildRead`-only) path keeps its current zero-ambient
strictness as the natural special case where all three allow-lists are `None`.

## Parity considerations

- **Message parity:** exec/env/net denials must abort with the *same* string on both
  backends. Add a differential test in `src/example_tests.rs` per denial (ungranted tool,
  ungranted key, ungranted host) and a `book/` example for the build-step surface.
- **The interpreter path is retained** as the oracle: `interpreter::run_build_step` is not
  deleted; it stays the reference that the new compiled path is differentially checked
  against (same as every other operation). The *production dispatch* stops choosing it.
- **No new per-op fast paths** (the standing rule): this is one general allow-list mechanism
  on `Capabilities`, consumed uniformly by the exec/env/net host functions — not a per-tool
  or per-key recognizer.

## Alternatives considered

1. **Leave it on the interpreter (status quo).** Rejected: it is the last production
   interpreter holdout and the only path whose behavior is never observed compiled, so a
   compiled-only divergence there is invisible — the exact risk the parity rule exists to
   kill.
2. **Coarsen build grants to bools to match today's `Capabilities`.** Rejected: it throws
   away the build model's confinement (a step could spawn any tool / read any env var),
   which is a security regression, not a simplification.
3. **A capability-carrying externref per build tool (cf. RFC-0005).** Heavier than needed:
   exec/env grants are small static allow-lists checked host-side by name; a string
   allow-list on `Capabilities` is sufficient and matches the proven `net_allow` design.

## Migration / rollout

1. Add `exec_allow`/`env_allow` to `Capabilities` (default `None` — inert for all current
   callers).
2. Guard `host_exec_run` and the env host import.
3. Add `run_build_step_compiled` (or generalize the sandboxed path); route `BuildGrants`
   allow-lists in; widen the `sandboxable` gate to all build steps.
4. Differential tests + a `book/` example; then delete the kind-based branch in
   `run_build_step_file` and update its comment (which currently asserts the WASM boundary
   "cannot itself enforce" the allow-list — no longer true).

No user-visible syntax or grant-declaration change; `build` steps keep declaring
`Build[Exec{…}]`/`Build[Env{…}]`/`Build[Net{…}]` exactly as today. The only observable
difference is that they now run on the production tier (faster, and covered by parity).
