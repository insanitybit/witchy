---
rfc: 0068
title: "Unify build caps onto the runtime capability set (finish Exec/Env refinement)"
status: proposed
created: 2026-07-06
related:
  - "0001 (the parity prime directive: interpreter = differential-testing oracle only)"
  - "0004 (self-hosted CLI — pm/coven now run compiled; this closes the remaining holdout)"
  - "0011/0012/0013 (capability refinement — runtime Exec/Env growing allow-lists IS this work)"
tracking:
---

# RFC-0068: Unify build caps onto the runtime capability set

> Provisional throughout. Code blocks are intentionally **not** tagged `witchy` so the
> doc-examples sweep does not compile pre-implementation snippets.

## Summary

The interpreter is meant to be the **differential-testing oracle only**; the compiled
WASM tier is the sole production run path. Two recent fixes removed the last two
production paths that still ran through the tree-walker: `witchy pm`/`coven-serve`
(compiled) and `witchy test` (compiled). **One deliberate holdout remains:** a `build`
step that needs `BuildExec`, `BuildNet`, or `BuildEnv` runs on the interpreter.

The framing matters. Build caps and runtime caps are **not two systems** — they are
already **one `Capabilities` struct** whose fields both carry each grant *and* gate
whether the matching host import is linked (`if caps.exec { link exec_run }`). Build's
Out/Read already live there (`build_out`, `build_read_roots`) and run compiled. The
reason Exec/Env don't is narrow: **runtime `exec`/`env` are all-or-nothing `bool`s,
while `net`/`dir` already carry allow-lists** (`net_allow`, `dir_roots`). Build's grants
are fine-grained (`exec_tools`, `env_keys`), so a bool can't represent them without
over- or under-granting — which is what forces the interpreter.

So the fix is not "add a build-enforcement subsystem." It is **finish runtime capability
refinement**: give `Exec`/`Env` allow-lists so they match `Net`/`Dir`, then let build
caps lower straight onto the unified fields. That upgrade is desirable on its own (an
`Exec[cc,ld]` runtime grant is exactly what RFC-0011/0012/0013 want); build is just the
first consumer. Confinement is preserved for free — linking is already caps-gated, so a
step gets only the imports its footprint grants.

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

The comment's claim — "the WASM boundary cannot itself enforce" the allow-list — is
**false**, and seeing why points at the real fix. Confinement on the compiled backend is
**caps-gated import linking**: `Runtime::spawn` does `if caps.exec { link exec_run }`,
`if caps.build_out.is_some() { link build_out_write }`, and so on. A host import a step
wasn't granted is simply *never linked* — the "no host import to call" the comment relies
on is produced by the very `Capabilities` struct build caps already share. Build's Out/Read
are fields on that struct (`build_out`, `build_read_roots`) and run compiled today. There
is no second system to unify with; there is one struct.

What's actually missing is **expressiveness on two of its fields**. `net`/`dir` carry
allow-lists; `exec`/`env` do not:

- `Capabilities.exec: bool` — all-or-nothing. `host_exec_run` does `Command::new(&prog)`
  with **no tool allow-list check**.
- `Capabilities.env: bool` — all-or-nothing. No per-key filter.
- `Capabilities.net_allow: Option<Vec<String>>` — **already fine-grained** (used by
  `coven-serve`/`witchy pm`).

The interpreter's `BuildGrants` carry the allow-lists the build model promises:

    BuildCap::Exec(exec_tools: Vec<String>)   // only these programs may be spawned
    BuildCap::Env(env_keys:  Vec<String>)     // only these keys may be read
    BuildCap::Net(net_hosts: Vec<String>)     // only these hosts may be reached
    BuildCap::Read(read_roots) / BuildCap::Out(out_dir)   // already compiled-sandboxable

So a step declaring `Build[Exec{cc}]` cannot map onto `Capabilities` today without either
(a) *over*-granting (`exec = true` → spawn *anything*, destroying the build model's
confinement) or (b) *under*-granting (`exec = false` → the step traps). That mismatch —
not a missing subsystem — is the whole holdout. Note the corollary: **`Build[Net]` needs
nothing new** — `net_allow` already exists, so a net-using build step could run compiled
today; only `Build[Exec]`/`Build[Env]` are genuinely blocked, and only on field
expressiveness.

### Why it matters

With pm/coven/`witchy test` migrated, `run_build_step_file`'s exec/env/net branch is the
**last deliberate interpreter invocation in a production path**. Closing it means the
interpreter is invoked *only* for: the parity oracle (`parity_check`), compile-time
const-eval (`comptime`/tagged literals), and benchmarks/tests. That is the invariant fully
realized — and it removes a second, subtler risk: a build step's behavior today is only ever
observed on the interpreter, so a build-step-only divergence on the compiled backend would
never be caught.

## Design

Upgrade runtime `Exec`/`Env` to carry allow-lists (making them consistent with `Net`/`Dir`),
then lower build caps onto the shared fields. This is a *runtime capability refinement* that
build consumes — not a build-specific mechanism. `Build[Net]`/`Build[Out]`/`Build[Read]`
already map onto existing fields; only `Build[Exec]`/`Build[Env]` need the upgrade.

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

The unified framing lets this land **incrementally**, smallest first:

1. **`Build[Net]` compiled — no new fields.** `net_allow` already exists; route
   `BuildCap::Net(net_hosts)` into it and admit net-only steps into the compiled path.
   This is a pure `sandboxable`-gate + routing change, shippable on its own.
2. **Add `exec_allow`/`env_allow` to `Capabilities`** (default `None` — inert for all
   current callers), and guard `host_exec_run` + the env host import against them. This is
   the runtime capability-refinement core (RFC-0011/0012/0013) and is independently useful
   beyond build.
3. **Route `BuildGrants` exec/env allow-lists in** (via `run_build_step_compiled` or a
   generalized sandboxed path) and widen the `sandboxable` gate to every build step.
4. Differential tests + a `book/` example per denial (ungranted tool/key/host); then delete
   the kind-based branch in `run_build_step_file` and its now-false comment. The
   `BuildOut`/`BuildRead`-only path remains as the natural special case (all allow-lists
   `None`).

No user-visible syntax or grant-declaration change; `build` steps keep declaring
`Build[Exec{…}]`/`Build[Env{…}]`/`Build[Net{…}]` exactly as today. The only observable
difference is that they now run on the production tier (faster, and covered by parity).
