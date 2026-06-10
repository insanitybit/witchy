# Implementation Plan: Build-Time Execution as a Capability

Status: **plan, not yet implemented.** This is the roadmap for building the
build-time half of the capability model. The *design* already exists in
[package-manager.md](package-manager.md) §4 (the footprint model) and §7.1
(build-time execution as a capability); this document turns that design into a
phased, code-grounded implementation plan.

## Why this, and the reframing

Runtime capability safety is already complete **by the type system**: capabilities
are unforgeable, enter only at `main`, and flow solely by argument, so a function
cannot exercise authority it was not handed, and a dependency cannot widen what it
demands without breaking its callers' type-checking. The runtime `witchy caps` /
`caps-diff` is therefore *reporting and pre-flight diffing over a guarantee the
types already provide* — useful for governance, but not a separate line of
defense.

The one axis that is **not** in your program's type-checked call graph is
**build-time execution**: code a rune runs while *being built* (codegen from a
schema, etc.). That is the npm `postinstall` / cargo `build.rs` attack surface.
witchy eliminates ambient build execution entirely (resolve/install run no rune
code), and §7.1 models the legitimate cases as the *same* capability machinery as
runtime — typed, statically computed, granted per-rune, lock-pinned, gated.

**So the audit/footprint feature should be framed as a supply-chain tool with two
axes, and build-time is the headline** — it is the part capability *analysis*
uniquely defends, beyond what types already enforce.

## Conventions this plan adopts (faithful to §7.1)

- **Build step = a witchy program with a `build` entrypoint.** A rune that ships
  one places it at `src/build.witchy` with `fn build(out: BuildOut, ...)`. It is
  validated like `main` (only build capabilities as parameters), runs in a
  zero-ambient sandboxed WASM actor, and its *only* product is generated `.witchy`
  source written through `out`.
- **Five build capability types**, a parallel set to the runtime caps, each
  attenuable cap-std style:
  - `BuildOut` — write generated source into this rune's own confined output
    sandbox. The only cap granted automatically.
  - `BuildRead` — read specific project files/dirs (rights/scope like `Dir`).
  - `BuildEnv` — read specific named env vars.
  - `BuildNet` — fetch from an explicit host allow-list.
  - `BuildExec` — invoke a specific named external tool (most sensitive; outputs
    hashed into the lock).
- **Per-rune grants in the consuming `witchy.toml`** under
  `[build.grants."ns/name"]`; safe by default (only `BuildOut` without a grant).
- **The gate (§10) extends to the build axis**: an upgrade whose build step newly
  demands a build cap is blocked until `--allow-build-cap` + a grant.

---

## Phase 1 — Static build footprint + the `build` entrypoint + caps reframe ✅ DONE

Compiler-only; no execution. Self-contained, fully testable, and unblocks the
rest. This is "audit, reframed around build-time" with nothing actually running.

*Implemented:* the five build cap types + `is_build_capability_type` +
`check_build_signature` + `build_entrypoint` (typeck); two-axis `Footprint`/
`FootprintDiff` with `build`/`build_added`/`build_widened` (capabilities.rs);
`witchy caps` prints a "Build-time footprint" section and `caps-diff` flags
"BUILD WIDENING" and exits non-zero (main.rs); tests in both modules.

**Type system (`src/typeck.rs`)**
- Add `BuildOut`, `BuildRead(DirRights)`, `BuildEnv`, `BuildNet(NetRights)`,
  `BuildExec` to the `Ty` capability variants and the `Type`→`Ty` mapping
  (mirroring `Dir`/`Net`). Decide rights parameterization: `BuildRead` reuses
  `DirRights`-style scoping; `BuildNet` reuses host-list scoping; `BuildEnv`/
  `BuildExec` carry a name list (new small rights type, or reuse a string set).
- Extend `is_capability_type` (line ~414) and the internal `is_capability`
  (Ty-level) to recognize the build kinds.
- Add `check_build_signature` (sibling of `check_main_signature`, line ~432):
  a `build` function may take only build capabilities; runtime caps / `List(String)`
  are rejected with a clear message. A module with no `build` fn is fine.

**Footprint analyzer (`src/capabilities.rs`)**
- Make `Footprint` two-axis: keep the existing entries as the *runtime* axis and
  add a parallel *build* axis (computed over the `build` entrypoint's signature).
  Likely: `Footprint { runtime: Vec<Entry>, build: Vec<Entry>, total_runtime,
  total_build }`, or tag each `Entry`/`CapSet` element with an axis.
- `host_cap` / `caps_in` classify the build kinds onto the build axis.
- `analyze` (line ~313) additionally walks the `build` entrypoint.
- `diff` / `FootprintDiff::widened` already operate on `CapSet` lattices — extend
  to report widening per axis (runtime widening vs. build widening are distinct,
  high-signal events).

**Tooling (`src/main.rs`)**
- `witchy caps`: print both axes, clearly labeled (e.g. a `runtime` section and a
  `build` section; omit the build section when there's no build step).
- `witchy caps-diff`: exit non-zero on widening of *either* axis; label which.

**Tests** (mirror the existing `capabilities.rs` tests): a `build` entrypoint's
footprint is the union of its build-cap params; pure `build` → empty build
footprint; `caps-diff` flags a build-axis widening; `check_build_signature`
rejects a runtime cap in `build`.

**Docs (stub):** note in package-manager.md that the build footprint is now
computed (Phase 1), execution still pending.

---

## Phase 2 — Sandboxed build execution

Make the `build` step actually run, confined.

**Soundness requirement for auto-execution (audit before and after) ✅ DONE.**
A build step cannot *modify* existing source — `BuildOut` is confined to a fresh
per-rune output sandbox (path-escape rejected, tested) and `BuildRead` is
read-only — but the source it *generates* is linked into the program, and
generated code can declare capability-typed signatures. So the pipeline
recomputes the rune's footprint over **shipped + generated** source and runs the
widening gate against the locked baseline; generated source that widens either
axis blocks exactly like a version bump would (e2e:
`build_steps_auto_run_and_generated_source_is_gated`). Defense in depth: a new
runtime demand also breaks the consumer's type-check at the call site, and
generated modules may not shadow std.

*Phase 2a done (interpreter path):* `interpreter::run_build_step(module, BuildGrants)`
mints the build caps for the `build` entrypoint from confined grants (a `BuildOut`
output dir, an optional `BuildRead` root, `BuildEnv` key allow-list, `BuildExec`
tool allow-list) and runs it; the build host builtins are confined via the same
`resolve`/`resolve_write` machinery as runtime Dir ops (`fetch_build` refuses for
now). `witchy build-step <file> [--out][--read][--env][--exec]` exercises one
directly.

*Auto-run done:* `witchy build`/`run` exclude a dependency's `build` module from
the consumer link (so two runes shipping one can't collide), execute it under the
manifest grants into `<project>/build-out/<rune>/`, link the generated `.witchy`
modules in under the usual std-shadowing/collision guards, and apply the
audit-before-and-after gate above. Remaining 2b: the WASM-sandbox execution path
(zero-ambient `Linker`) for the hard isolation guarantee, `BuildNet`, build-output
caching (§7.2; today steps re-run per build), and multiple `read` roots.

**Runtime (`src/runtime.rs`)**
- Extend `Capabilities` with the build-time grants and add build host functions,
  linked into the per-actor `Linker` exactly like the runtime host fns. Start
  with `BuildOut`: a `build_out_write(rel, bytes)` that writes into a confined
  per-rune output directory (reuse `interpreter::resolve_write` confinement, as
  the runtime Dir host fns already do — see `host_dir_write`).
- A build actor is spawned with **zero ambient authority**: only the build host
  functions for the caps it was granted are linked; everything else is absent, so
  it physically cannot call them (same property as the runtime sandbox).

**Driver (`src/main.rs` `build`/run pipeline)**
- Before compiling the consumer, for each rune with a `src/build.witchy`: compile
  it to WASM, run `build(...)` in the sandbox with its granted caps, collect the
  generated `.witchy` from its output sandbox, and feed it into the normal
  parse→link→type-check pipeline alongside the rune's hand-written source.
- Output is cached by (input hash + build footprint + grants) for determinism
  (§7.2); deterministic steps rebuild for free.

Then add the remaining host functions, each attenuated: `BuildRead` (confined
Dir read), `BuildEnv` (named keys only), `BuildNet` (host allow-list), `BuildExec`
(named tool; content-hash outputs into the lock).

---

## Phase 3 — Manifest grants + lock + the gate ✅ LARGELY DONE

Wire the safe-by-default grant model and the widening gate.

*Status correction (audit):* most of this phase **already existed** in the PM —
it was built to the §7.1 spec ahead of the language types, and Phase 1's types
made it bite. Verified working end to end: `[build.grants."name"]` parsing with
`read`/`exec`/`net`/`env` allow-lists; `witchy.lock` recording per-rune
`runtime_footprint` + `build_footprint` + `determinism` + content hash; the
`add`/`update` gate blocking on build-axis widening with `--allow-build-cap`;
`witchy audit`/`why-cap` reporting both axes. Added on top: **default-deny on
execution itself** — a rune that ships a build step at all is refused until the
grants section exists (an empty section accepts execution with only `BuildOut`).
Staging cooldowns are now built too: records carry a **signed** `released_at`
(stamped at promote in both the Rust registry and the witchy coven — the signing
payload gained a `released_at=` line in all three implementations), and a fresh
release is not resolvable until `WITCHY_COOLDOWN_SECS` (default 72h) passes,
unless `add`/`update` is run with `--allow-fresh`. Locked versions are
unaffected (like yank, the cooldown gates *new resolution* only). Still pending
here: coven surfacing the build footprint at the promotion checkpoint.

**Manifest (`src/pm/` + `witchy.toml` parsing)**
- Parse `[build.grants."ns/name"]` granting attenuated build caps to a specific
  rune's build step. Absent ⇒ that rune's build step gets only `BuildOut`.
- A rune whose build step *demands* an ungranted cap **fails the build**, naming
  the rune and the demanded cap ("rune `acme/foo` wants `BuildNet` at build time;
  grant it in `[build.grants.\"acme/foo\"]` or it cannot build").

**Lock + gate (`src/pm/`, coven)**
- The lockfile records each rune's `build_footprint` (demanded) and the grants in
  effect; the build runs only if grant ⊇ demand.
- Extend block-on-widening (§10) to the build axis: `witchy add`/`update` block
  when a new version's build step newly demands a build cap; `--allow-build-cap`
  to accept, which also adds the grant.
- coven recomputes the build footprint server-side at publish (never trusts
  declared metadata), and the promotion checkpoint (§8.1) surfaces "wants `exec
  protoc` at build time" before download.

---

## Phase 4 — Docs reframe

Make the framing match the model.

- **README**: the supply-chain section leads with "runtime authority is enforced
  by the type system; the mechanism capability *analysis* uniquely adds is the
  build-time footprint + gate." Adjust the three-bullet pitch accordingly.
- **Book** (`book/src/`): the capabilities chapters and the packages chapter get
  the same reframe; add a short "Build-time capabilities" section once Phase 2
  lands so examples are real.
- **package-manager.md**: promote §7.1 from "designed" to "implemented" as phases
  land; keep the threat table (T1/T5) accurate.

---

## Resolved decisions

1. **Build-cap rights: kind in the type, names in the grant, names advisory in the
   rune manifest.** The five build caps are **nullary nominal types** — no name
   lists in the type system. Three layers: the *type* carries the kind (sound,
   unfakeable; §4.4 gates on kind); the *consumer grant* in `witchy.toml` carries
   the enforced names (`exec = ["protoc"]`), which the sandbox host functions bind
   to; the rune's own `[build] requires` is *advisory* metadata for the
   footprint/promotion UX ("wants to exec `protoc`"). Rejected names-in-type
   (`BuildExec["protoc"]`): consistent with `Dir[Read]` but pushes arbitrary
   string literals into type-rights position for a benefit the grant layer must
   enforce anyway. `BuildRead` is likewise nullary — its confined directory is the
   grant, not the type.
2. **Entrypoint: `fn build(out: BuildOut, ...)` in `src/build.witchy`.** Reserved
   filename, mirrors "the run entry is `fn main`." For the *static* analysis
   (Phase 1, which operates on the linked module and can't see file identity), the
   build entrypoint is the top-level `fn build` whose first parameter is
   `BuildOut`; the file convention governs which source the Phase-2 driver
   compiles and runs.
3. **Output sandbox: per-rune confined dir under the build cache,**
   `<store>/build-out/<ns/name>@<ver>-<inputhash>/`, written via the existing
   `resolve_write` confinement, namespaced per rune, linked into the consumer's
   compile as that rune's own generated `src/`.
4. **Cache key = hash of** (rune source + `build.witchy`) ⊕ build-footprint ⊕
   grants-in-effect ⊕ `build_inputs` (content hashes of any `BuildExec`/`BuildNet`
   outputs). Deterministic steps rebuild for free; impure ones pinned by output
   hash (§7.2).
