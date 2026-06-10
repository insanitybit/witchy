# The witchy package manager — design spec

Status: draft / design phase (2026-06-04)
The registry is **coven**; a published package is a **rune**. The management
commands (`witchy add`, `witchy publish`, …) fold into the existing `witchy`
binary, the way `cargo` extends `rustc`. Manifest `witchy.toml`, lockfile
`witchy.lock`. Names aside, everything below is the substance.

---

## 0. One-sentence thesis

A package manager for witchy must be a tool that **moves bytes and verifies
them** — it must *never* become a new source of ambient authority. Authority
stays a capability concept: granted at `main` for runtime, and granted
explicitly per-rune for build time. The tool's only jobs are **integrity,
reproducibility, and making every rune's capability footprint — runtime *and*
build-time — legible and gated**. Everything below follows from that.

---

## 1. Why witchy can be "safe by default" when npm/pip/cargo cannot

The overwhelming majority of real supply-chain compromises exploit **ambient
authority that the packaging tool itself confers**:

- install/build-time code execution: npm `postinstall`, pip `setup.py`,
  cargo `build.rs`, proc-macros — all run with the *full ambient authority of
  the developer's machine*;
- runtime code with full ambient authority: a dependency reads
  `~/.aws/credentials` or `~/.ssh/id_rsa` and exfiltrates it, because *any* code
  in the process can open *any* file and *any* socket.

witchy already structurally removes the runtime leg, via mechanisms that exist
today:

- **Imports are declarations-only** (`src/linker.rs`). `import X` brings names
  into scope, runs no code, confers no authority.
- **Authority enters only at `main`** (`src/interpreter.rs:350`, `root_cap_for`).
  `main`'s typed parameters (`Console`, `Dir`, `Net`) are minted by the host;
  capabilities are unforgeable values that propagate *only* as function
  arguments and are visible in types. Referencing an ungranted capability is a
  compile-time error.
- **Actors (WASM VMs) are the hard boundary** — a per-actor `Linker` links only
  the host functions that actor was granted.

The build leg does not exist yet — and rather than ban it forever (and risk an
insecure bolt-on later), this design **models build-time execution as the same
kind of capability**: a build step is a sandboxed actor that holds only the
build-time authority the consuming project explicitly granted it (§7.1). So both
legs of the classic attack are governed by one mechanism.

---

## 2. Threat model

### In scope (attacks we defend against)

| # | Attacker / vector | Defense |
|---|---|---|
| T1 | Install/build-time code execution (postinstall, `build.rs`) | Resolve/install run **no rune code at all**. A rune's *build step*, if it ships one, runs as a **sandboxed actor with zero ambient authority** — only the build-time capabilities the consuming project explicitly granted *that rune*, lockfile-pinned and block-on-widening. Default grant is a confined output dir; net/exec/env/read are denied until granted. (§7.1) |
| T2 | Malicious dependency exfiltrates at runtime | Capability model: a dep has zero ambient authority. It acts only on capabilities the caller passes to its functions — visible in types, attenuable, bounded. (§1) |
| T3 | Typosquatting / dependency confusion | Registry-qualified, namespaced deps; **no implicit public-registry fallback**. Every dep names its source registry; the lock pins registry + content hash. (§5, §6) |
| T4 | Registry compromise, metadata tampering, rollback, freeze | TUF-style signed metadata: `root`/`targets`/`snapshot`/`timestamp` roles. Snapshot prevents mix-and-match & rollback; timestamp prevents freeze. (§8) |
| T5 | Maintainer account takeover → malicious new version | The **capability footprint is typed, computed, and diffable**. A version that newly demands a capability kind (runtime *or* build-time) is blocked until re-approved; release additionally requires the §8.1 promotion gate. Blast radius bounded by attenuation. (§4, §10, §8.1) |
| T6 | Malicious deep transitive dependency | Footprints are **computed and transitive**. A child can never demand more authority than flows down to it. `witchy audit` shows the whole tree's *maximum* authority as a small, exact set. (§4, §11) |
| T7 | Self-asserted-but-false capability metadata | The registry and client **recompute** the footprint (runtime and build) from source and reject any mismatch with the declared one. The footprint is verifiable, never trusted. (§4, §8) |
| T8 | Compromised publish credential / CI token silently releases malware | **Two-phase publish: upload ≠ availability.** An uploaded version is *staged* and not resolvable; a human must *promote* it in a separate, out-of-band, second-factor event — distinct identity/credential from the uploader. A stolen push token can stage but cannot release. (§8.1) |

### Out of scope / residual risk (named explicitly)

- **Microarchitectural side channels** (Spectre etc.). The WASM boundary is a
  *logical* sandbox, not Spectre-proof. Unchanged by this design.
- **User grants authority to a malicious rune.** If your `main` hands a malicious
  rune the root `Net`, or your manifest grants its build step `BuildExec`, it can
  use them. The tool makes the demand loud and blocks silent widening;
  attenuation shrinks blast radius — but the final grant is the user's call.
- **Bugs in the witchy compiler / runtime / linker.** The trusted computing base.
  Footprint soundness depends on the type checker correctly identifying
  capability types.
- **Social engineering** into approving a widening/grant, or into adding a
  malicious rune at all. The tool raises cost and visibility; it can't eliminate
  human approval.
- **Registry availability** (DoS). We defend integrity, not uptime.

---

## 3. Core principle (restated as design constraints)

1. **Rune code never runs with ambient authority.** Resolution and install run
   no rune code at all. A rune's *build step*, if present, runs only as a
   confined actor holding exactly the build-time capabilities the consuming
   project granted it — default: nothing but a sandboxed output dir. (§7.1)
2. **The tool never grants authority; the user's manifest does, explicitly.**
   Adding a dependency grants it nothing. It gains runtime power only when the
   user's code passes it a capability, and build-time power only via an explicit
   per-rune grant in `witchy.toml`.
3. **The capability footprint is a first-class, computed, gated artifact** —
   along two axes, runtime and build-time.
4. **Everything is content-addressed and reproducible.** Same lock → same bytes
   and same generated source, offline.

---

## 4. The capability footprint — the heart of the design

### 4.1 Definition

The **capability footprint** of a rune is the set of capability *kinds* its
public API requires a caller to supply for it to perform effects. It has two
axes: the **runtime footprint** (what its exported functions/actors demand of
`main`'s grants) and the **build footprint** (§7.1, what its build step demands
of the consuming project). This section describes the runtime axis; the build
axis is computed identically over the build entrypoint's signature.

Because authority cannot be forged and enters only at `main`, a non-`main` rune
can never hold a runtime capability it did not *receive through its public
surface*. Therefore the footprint is **exactly** the set of capability types
appearing in the rune's exported signatures — a *tight*, *sound* upper bound.
There is no hidden authority to miss.

### 4.2 Capability types

Runtime base kinds (from the runtime today): `Console`, `Dir`, `Net`, plus the
handle types `Socket`, `Subject`. A *user* type is "capability-tainted" if it
transitively contains a capability-typed field; constructing it still requires
the underlying capability to be passed in, so taint propagates through types.
Build-time kinds (`BuildOut`/`BuildRead`/`BuildEnv`/`BuildNet`/`BuildExec`) form
a parallel set on the build axis (§7.1).

### 4.3 Computation (static, decidable)

Over the linked + type-checked AST of a rune:

```
footprint(rune):
  caps = {}
  for each PUBLIC item (exported fn / actor / constructor, or the build entrypoint):
    for each parameter / field type T in its signature:
      caps |= capability_kinds_reachable(T)   # base caps + tainted-type closure
  return classify(caps)                        # runtime kinds and/or build kinds
```

`capability_kinds_reachable` reuses the type checker's existing knowledge of
which types are capabilities.

**Transitivity is automatic.** If rune A's exported function calls B and passes a
capability through, A must itself have received that capability in its own public
signature (it cannot mint one — only `main` mints). So computing the footprint
from the public surface already accounts for everything A can cause B, or any
spawned actor, to do.

### 4.4 Granularity: static kind vs. runtime scope

- **Static footprint = capability *kind*** (`Net` yes/no, `BuildExec` yes/no).
  This is what the package manager gates on. "This logging rune now wants `Net`"
  / "this rune now wants to `exec` at build time" are the high-signal, diffable
  events.
- **Runtime/grant scope = attenuated instance** (which hosts, which directory
  subtree, which named tool). Decided by `main` at runtime, or by the per-rune
  build grant in `witchy.toml`, invisible in types.

The two compose: the tool governs *which kinds of authority* a rune may ever
demand; `main` / the manifest grant governs *how narrow* each granted instance
is.

### 4.5 The widening order

Footprints are subsets of the capability-kind lattice (runtime and build axes
tracked separately). An upgrade **widens** iff `footprint(new) ⊋ footprint(old)`
on either axis — it demands a capability kind the locked version did not.
Widening is the gated event (§10).

---

## 5. Manifest — `witchy.toml`

```toml
[rune]
name    = "acme/http-client"          # namespace/name (T3: explicit namespace)
version = "1.2.0"
source  = "git+https://github.com/acme/http-client"   # provenance anchor (§8)

[capabilities]
# Declared footprint. NOT trusted: the tool recomputes from source and errors on
# mismatch (T7). Serves as a reviewable, diffable contract in code review.
runtime = ["Net"]
build   = []                          # this rune ships no build step

[dependencies]
"std/json" = { version = "^2.1", registry = "coven" }
"acme/url" = { version = "1.0",  registry = "coven" }
# path / git deps allowed for local dev; still hashed + footprinted in the lock:
"local/util" = { path = "../util" }

# Build-time grants the CONSUMING project hands to specific deps' build steps.
# Safe by default: a dep's build step gets nothing here but a confined output dir.
[build.grants."acme/protobuf-gen"]
read = ["proto/"]                     # BuildRead, confined to ./proto
exec = ["protoc"]                     # BuildExec, only the named tool
# net / env absent => denied
```

Key points:
- Every dependency names its **registry** explicitly. No implicit fallback to a
  different registry (kills dependency confusion, T3).
- `[capabilities]` is a *declared* contract the toolchain **verifies by
  recomputation** on both axes; divergence is a hard error.
- `[build.grants."ns/name"]` is the *only* way a dependency's build step gains
  authority — explicit, per-rune, attenuated, and recorded in the lock.

---

## 6. Lockfile — `witchy.lock`

Pins every node in the resolved graph. Generated/updated only by `add`/`update`;
consumed offline by `build`/`run`.

```toml
[[rune]]
name      = "acme/http-client"
version   = "1.2.0"
registry  = "coven"
hash      = "sha256:9f86d081..."        # canonicalized source tree (§7)
runtime_footprint = ["Net"]             # computed; the gate diffs against this (§10)
build_footprint   = []                  # this rune runs no build step
provenance = "sigstore:...|git:commit=abc123|signer=acme"   # §8

[[rune]]
name      = "acme/protobuf-gen"
version   = "0.4.0"
registry  = "coven"
hash      = "sha256:..."
runtime_footprint = []
build_footprint   = ["BuildRead", "BuildExec"]   # demanded by its build step
build_grants      = { read = ["proto/"], exec = ["protoc"] }   # in effect (§7.1)
build_inputs      = ["sha256:..."]      # hashed exec/fetch outputs => reproducible
provenance = "..."
```

The `*_footprint` fields are the baselines the capability gate compares the next
resolution against (§10). Hashes make builds reproducible and tamper-evident;
`build_inputs` pins anything a granted `BuildExec`/`BuildNet` produced so
rebuilds are deterministic; provenance ties bytes to public source history.

---

## 7. Content addressing, the store, and the build

- A rune's identity is the **sha256 of its canonicalized source tree** (sorted
  paths, normalized line endings, no timestamps). Versions are immutable; the
  same version can never serve different bytes.
- Downloaded runes live in a global content-addressed store
  (`~/.witchy/store/<hash>/`), shared across projects, append-only.
- `witchy vendor` materializes the resolved tree into the project for fully
  offline, auditable builds.
- **The build is: parse → link → type-check → (interpret | codegen) of the
  *user's* program, with dependency *source* linked in.** No rune code executes
  during resolution or installation. Build-step execution, when a rune ships one,
  is governed entirely by §7.1 — sandboxed and capability-gated, never ambient.

### 7.1 Build-time execution as a capability (the model for "when it's needed")

> **Implementation status.** Built — see
> [build-time-execution-plan.md](build-time-execution-plan.md). The five build
> capability *types* and the `build` entrypoint; the two-axis footprint
> (`witchy caps`/`caps-diff` report and gate the build axis); all five build
> capabilities executing confined; `[build.grants."name"]` in `witchy.toml` with
> **default-deny on execution itself** (a rune that ships a build step is refused
> until the grants section exists); `build_footprint` in the lockfile and the
> `add`/`update` gate blocking build-axis widening (`--allow-build-cap`); build
> steps **auto-run during `witchy build`** with the footprint **recomputed over
> shipped + generated source and gated against the locked baseline** (generated
> code cannot smuggle in authority); deterministic build-output **caching**;
> **staging cooldowns** (signed `released_at`, 72h window, `--allow-fresh`); the
> promotion checkpoint surfacing the absolute footprint; and the zero-ambient
> **WASM-sandbox execution path** for deterministic steps (`BuildExec`/`BuildNet`
> steps run on the capability-sound interpreter, where the sandbox adds no
> isolation over the allow-list). The only residual is hardening depth, not
> capability coverage.

Assume consumer-side build execution *will* eventually be required (generating
witchy source from a schema, etc.). Model it now so that when it lands it is
**safe by default**, governed by the *same* machinery as runtime: typed,
statically computed, lockfile-pinned, explicitly granted per-rune, and gated.

- **Build steps are actors.** A rune may ship a build step — a witchy program
  with a `build(...)` entrypoint. The toolchain runs it during `witchy build`,
  *before* the consuming program compiles, **inside a sandboxed WASM VM** using
  the exact same per-actor `Linker` isolation as runtime actors. It has **zero
  ambient authority**: only the build-time host functions explicitly granted to
  it are linked.
- **Its only product is source.** A build step emits generated `.witchy` source
  (and data) into a confined per-rune output sandbox, which then flows into the
  normal parse→link→type-check pipeline. It cannot touch the project tree, the
  store, or other runes' outputs — in particular it **cannot modify existing
  source** (`BuildOut` is write-confined to the fresh sandbox; `BuildRead` is
  read-only). And because *generated* source is still source, the pipeline
  recomputes the rune's footprint over shipped **plus generated** code and runs
  the widening gate on the result: a build step cannot smuggle authority into
  the program by generating capability-hungry code.
- **Build-time capabilities** — a distinct, enumerated set, each attenuable
  (cap-std style):
  - `BuildOut` — write generated source into *this rune's own* output sandbox.
    The only capability granted automatically; confined, cannot escape.
  - `BuildRead(Dir)` — read specific project files/dirs (e.g. a `.proto`).
    Confined Dir; explicit.
  - `BuildEnv(keys)` — read specific, *named* env vars (e.g. `TARGET`). Never all
    of env.
  - `BuildNet(hosts)` — fetch from an explicit host allow-list. Off by default.
  - `BuildExec(tools)` — invoke a specific *named* external tool (e.g. `protoc`).
    The "native toolchain" escape hatch; most sensitive; its outputs are
    content-hashed into the lock (`build_inputs`).
- **Static reasoning.** The toolchain computes the **build footprint** from the
  `build(...)` entrypoint's typed parameters — exactly like §4, on the build
  axis. A sound upper bound on what the build step can do.
- **Explicit per-rune grants (default-deny, including execution itself).** A
  rune that ships a build step *at all* is refused until the consuming project
  writes a `[build.grants."ns/name"]` section (§5) — you consent to **any** code
  execution before you consent to *safe* code execution. An empty section is
  that consent and permits only the confined `BuildOut` sandbox; every further
  capability must be named in it (`read`/`exec`/`net`/`env` allow-lists — env
  access is per-*named-variable*, never the whole environment). Adding a rune
  whose build step *demands* an ungranted capability **fails the build** and
  surfaces the demand. A malicious rune that "needs network at build time" stops
  cold.
- **Lockfile-pinned + gated.** The lock records each rune's `build_footprint`
  (demanded) and the grants in effect. The build runs only if grant ⊇ demand.
  The **block-on-widening** gate (§10) extends to the build axis: an upgrade
  whose build step newly demands a build capability is blocked until explicitly
  re-approved (`--allow-build-cap`) and the grant is added. `BuildExec`/
  `BuildNet` outputs are hashed into `build_inputs` so rebuilds are reproducible.
- **Reproducibility & the preferred path.** Build outputs are cached by
  (input hash + build footprint + grants); deterministic steps rebuild for free.
  The **safest, preferred** path remains **authoring-time codegen**: the rune
  author runs the build once and *vendors the generated source* into the
  published rune, so consumers get plain reviewable source and run *no* build
  step. Consumer-side build execution is the escape hatch for when vendoring
  genuinely can't work — and even then it is confined, enumerated, granted
  per-rune, and gated.
- **Publish/promote interaction.** A rune's build footprint is published metadata
  (recomputed server-side, §8) and shown at the promotion checkpoint (§8.1):
  consumers see "this rune wants to `exec protoc` at build time" *before*
  download, and a human vouches for it before release.

The same sentence now governs runtime and build time: **authority is typed,
computed, granted explicitly, pinned in the lock, and blocked from widening
silently.** This is strictly stronger than cargo's `build.rs` (ambient,
unauditable).

### 7.2 Build determinism — tiered by footprint

Reproducibility is far more tractable here than in ambient-authority ecosystems,
because the usual sources of build nondeterminism (clock, filesystem order, full
env, network, randomness) are exactly what the capability sandbox removes. Core
WASM is deterministic by construction, and the host *implements every build
capability*, so it can make nondeterminism nearly inexpressible. The build
footprint therefore doubles as a **determinism class**, and the policy is tiered
by it rather than one-size-fits-all:

- **Guaranteed tier** — build footprint ⊆ `{BuildOut, BuildRead, BuildEnv,
  BuildNet}` with no `BuildExec`. Determinism is **enforced**, and the host makes
  violations hard to even write:
  - `BuildRead` returns directory entries **sorted**, with no mtimes/inode order;
  - `BuildEnv` exposes only declared keys, whose values are **pinned** in the lock
    (`build_inputs`) and treated as recorded inputs, not hidden state;
  - `BuildNet` is **content-addressed**: the fetched bytes' hash is pinned, and a
    rebuild whose fetch doesn't match the pin hard-fails (Nix fixed-output /
    Go-checksum-DB style) — the network is a pinned input, never trusted live;
  - witchy controls its own collection iteration order, so "hashmap order leaks
    into generated output" simply cannot happen.
  For this tier the toolchain can effectively *guarantee* byte-identical output
  and `build` may verify it (optional double-build diff) at low cost.

- **Pinned-only tier** — build footprint includes `BuildExec`. Once a build shells
  out to a native tool (`protoc`, …) it inherits the full general
  reproducible-builds problem (that tool may embed timestamps, read
  `/dev/urandom`, depend on its own version/locale). witchy **cannot guarantee**
  determinism here, so it does **not** enforce it. Instead it **pins and
  verifies**: record the build's output hash (and any `BuildExec` outputs) in
  `build_inputs` on first build, and **fail loudly on drift** thereafter — a
  lockfile for the *output*, trust-on-first-build. The rune is flagged
  `reproducibility: pinned-only (uses exec)`.

The class is computed, not asserted, and surfaced by `coven` metadata and
`witchy audit` — so "is every build in my tree reproducibility-guaranteed?" is a
first-class, answerable question, and adopting a `BuildExec` rune is a visible,
gated downgrade from guaranteed to pinned-only.

### What about the other classic uses of build scripts?

- **Codegen** is the case §7.1 generalizes: prefer authoring-time vendoring of
  generated source; fall back to a granted, sandboxed build step when needed.
- **Native / host functionality at *runtime*** : a rune *cannot* add a runtime
  host function. New runtime effects are the runtime's job and arrive as
  runtime capabilities, never as rune build artifacts. (`BuildExec` invokes
  external tools at *build* time only, under an explicit named grant, and its
  outputs are hashed — it does not extend the runtime sandbox.)
- **Macros / metaprogramming**: if witchy adds them, model them as a build-time
  capability too — a sandboxed, hygienic, source→source transform under the
  §7.1 grant machinery — never arbitrary host code. (§13)

---

## 8. coven — the central, signed registry

A hosted registry, hardened with **The Update Framework (TUF)** roles:

- **root** — the trust anchor. Offline keys, pinned in the toolchain, rotatable
  via the root role itself. Delegates to the roles below.
- **targets** — signs the actual rune targets (their content hashes). Delegated
  to **per-namespace maintainer keys**: publishing a version requires a
  signature chaining from the namespace's delegated key.
- **snapshot** — signs the consistent set of all current target metadata.
  Prevents mix-and-match and **rollback** of any individual rune (T4).
- **timestamp** — short-lived signature over the snapshot. Prevents **freeze**
  attacks (a stale mirror can't pretend nothing changed) (T4).

Additional guarantees:

- **Provenance (SLSA-style).** Each published version binds the registry bytes to
  a public source repo + commit + publisher identity. Since runes are pure
  source, provenance reduces to "these bytes equal the published source at commit
  C, signed by the namespace owner."
- **Keyless signing (Sigstore/OIDC) preferred** so maintainers don't manage
  long-lived keys; identity is an OIDC subject recorded in a transparency log.
- **Namespaces are owned and registered.** `@acme/*` belongs to a verified owner;
  this plus explicit registry qualification (§5) closes dependency confusion.
- **Both footprints are published metadata**, recomputed and **enforced
  server-side**: coven rejects an upload whose source's computed runtime/build
  footprint disagrees with the declared `[capabilities]` (T7). Consumers see
  trustworthy footprints *before* downloading.
- **Immutability + advisory yank.** A version's bytes never change. A version may
  be *yanked* (excluded from new resolutions, flagged by `audit`) but existing
  locks still resolve it (reproducibility). Nothing is ever deleted.

### 8.1 Two-phase publish: stage → promote (release)

**Uploading a version does not make it downloadable.** Releasing is a deliberate,
human, out-of-band, second-factor act — separated from the upload — so that
compromise of the *publish* path alone cannot reach a single consumer (T8).

Version lifecycle:

```
        upload (publish key / CI token)        promote (human + 2FA, out of band)
  ───────────────────────────────────────► staged ───────────────────────────────► released ──► (yanked)
                                              │                                         │
                          not resolvable by any client                        resolvable; in snapshot
```

- **Staged** is the default landing state of `witchy publish`. The bytes are
  stored (immutable, content-addressed), both footprints computed and verified,
  but the version is **invisible to resolution** — `add`/`update` never select
  it. The publisher may install it explicitly for their own testing
  (`--include-staged`), writing a local-only lock entry, never the coven snapshot.
- **Promotion** is a distinct, signed **release-role** assertion — separate from
  the targets/upload signature — that flips a staged version to *released* and
  only then includes it in the TUF **snapshot** clients trust. Until promotion,
  the released snapshot does not reference the version at all, so a verifying
  client cannot even see it as a candidate.

Promotion requirements (the "double confirmation"):

1. **Out of band, interactive, second factor.** Promotion happens through a
   channel *separate from the automated upload* — a coven dashboard or an
   explicit `witchy promote` that triggers a challenge — and requires a second
   factor (WebAuthn/passkey or hardware token preferred; TOTP minimum). It is
   explicitly **not** a flag on the same CI push that uploaded the bytes.
2. **Separation of duties.** The promoting identity is authenticated
   independently of the upload credential. Per-namespace policy may require the
   promoter to be a *different human* than the uploader, and may require **N-of-M
   approvals** for sensitive namespaces.
3. **Footprints shown at the checkpoint.** The promotion UI/CLI displays the
   computed runtime *and* build footprints and their **delta vs. the
   currently-released version** (§4). The human promotes *with knowledge of* any
   capability widening — promotion is where a person consciously vouches for a
   footprint change before it reaches anyone.
4. **Non-repudiable audit.** Every promotion is recorded in the transparency log
   (who, when, factor type, footprints at promotion). Maintainers can monitor for
   unexpected promotions — detection even if both factors leak. And the
   **staging cooldown** (implemented, consumer-side): every record carries a
   signed `released_at`, and a freshly released version is not resolvable until
   its window passes (`WITCHY_COOLDOWN_SECS`, default 72 hours) unless the
   consumer explicitly accepts it with `--allow-fresh` — so a compromised
   release cannot be consumed the moment it lands, and the window cannot be
   erased by metadata tampering (the stamp is under the record signature).

This composes with the rest: a hijacked maintainer (T5) or a stolen CI token
(T8) can stage a malicious version, but releasing it still demands a separate
human, a second factor, and a look at the footprint deltas — a hard, logged,
out-of-band checkpoint rather than an automatic consequence of a push.

---

## 9. Resolution

- PubGrub-style version-constraint resolution over the dependency graph.
- Fully **offline given a lockfile**; the network is touched only by explicit
  `add` / `update` / `fetch`.
- **Hermetic and authority-confined.** The resolver executes no rune code. If/
  when the package manager is itself written in witchy, it runs as an actor whose
  only capability is a `Net` attenuated to the coven host — eating our own dog
  food. Even as a Rust subcommand, its network access is scoped to the configured
  registry.

---

## 10. The capability gate — **block on any widening**

The chosen enforcement posture: **a resolution that widens any rune's footprint
(runtime *or* build) relative to the lockfile hard-fails until explicitly
re-approved.**

Flow:

1. On `add` / `update`, compute the runtime and build footprints of every rune in
   the *proposed* resolved tree.
2. Diff against `witchy.lock`'s recorded footprints (and, for a brand-new dep,
   against the empty set / the user's expectation).
3. If any rune's footprint **widens** (gains a capability kind on either axis):
   - **Abort by default.** Print a precise delta, e.g.:
     ```
     BLOCKED: acme/logger 1.4.0 -> 1.5.0 widens capability footprint
       + Net   (runtime: newly demands network access)
     This logging rune did not previously require Net.
     Re-run with:  witchy update acme/logger --allow-cap Net

     BLOCKED: acme/protobuf-gen 0.4.0 -> 0.5.0 widens build footprint
       + BuildNet   (build: newly wants to fetch over the network)
     Re-run with:  witchy update acme/protobuf-gen --allow-build-cap BuildNet
                   and add a [build.grants] entry naming the allowed hosts.
     ```
   - Proceeding requires an explicit, per-capability ack (`--allow-cap` /
     `--allow-build-cap`), which records the new footprint in the lock as the new
     baseline; a build widening also requires the matching `[build.grants]` entry.
4. A resolution that does **not** widen any footprint proceeds without prompting.

This makes "a hijacked maintainer adds exfiltration" a wall, not a whisper:
exfiltration needs `Net`/`Dir` at runtime or `BuildNet`/`BuildExec` at build time
— each a typed footprint change the gate refuses to cross silently. Narrowing or
unchanged footprints are always free.

---

## 11. CLI surface

```
witchy new <name>            scaffold a rune + witchy.toml
witchy init                  add a manifest to an existing project
witchy add <pkg>[@ver]       resolve, show footprint delta, BLOCK on widening, write lock
witchy build                 offline build from lock; verify every hash; run granted,
                             sandboxed build steps only (§7.1); no ambient exec
witchy run                   build + run the user's program
witchy update [pkg]          re-resolve within constraints; same capability gate
                             (--allow-cap / --allow-build-cap to accept a widening)
witchy audit                 print the tree's aggregate + per-rune runtime AND build
                             footprints + determinism class (guaranteed vs.
                             pinned-only); flag yanked deps, weak provenance, drift
witchy why <pkg>             explain why a dep is in the tree (path)
witchy why-cap <Kind>        trace which rune(s) introduce a capability kind
witchy vendor                materialize the content-addressed store (and vendored
                             generated source) in-repo
witchy verify                re-verify lock against store + coven signatures
witchy publish               author flow: hash, recompute footprints, sign, upload
                             -> lands STAGED (not resolvable) (§8.1)
witchy promote <pkg>@<ver>   out-of-band, second-factor release of a staged version;
                             shows runtime + build footprint deltas before confirming
```

The headline audit story: because footprints are computed and transitive,
`witchy audit` answers **"what is the maximum authority — at runtime AND at build
time — anything in my entire dependency tree could exercise?"** with a small,
exact set — and *block on widening* guarantees that set cannot grow without an
explicit, recorded approval. No ambient-authority ecosystem can offer this.

---

## 12. Trust bootstrapping & keys

- coven's **root public key ships pinned in the toolchain**; rotation goes through
  the TUF root role.
- Maintainer identity via **Sigstore/OIDC keyless** signing by default
  (transparency-logged), with optional long-lived keys for offline publishers.
- First resolution of a registry records its verified root in the lock;
  subsequent runs verify the full role chain before trusting any metadata.

---

## 13. Open questions

- **Macros / metaprogramming.** If witchy adds them, model them as a build-time
  capability (§7.1): hygienic, sandboxed, capability-free source→source under the
  per-rune grant machinery. Never arbitrary host code. (Otherwise T1 reopens.)
- **Capability *scope* in metadata.** Should coven record coarse scope hints
  (e.g. "Net to a single host", "exec only protoc") beyond kind? Useful for
  review, but scope is fundamentally a grant decision; risk of false assurance.
  Lean: gate at kind granularity; surface scope as advisory author documentation.
- **Component Model / WIT migration.** Per the runtime roadmap, witchy will move
  to the Component Model, where a **WIT `world` *is* a capability manifest** and
  resource handles are capability tokens. Both footprint axes map onto WIT worlds
  (runtime imports + a build-time world). Design the metadata now to be
  forward-compatible.
- **Stdlib distribution.** Today std modules are bundled via `include_str!`.
  Decide whether std ships in-toolchain (current) or becomes coven runes under a
  reserved `std/` namespace with the same signing/footprint machinery.

---

## 14. Phased implementation plan

1. **Manifest + lockfile + content-addressed local store** (no network).
   `witchy.toml`, `witchy.lock`, path/git deps, hashing, offline `build`/`run`.
   Resolution from local sources. (Proves the plumbing without trust machinery.)
2. **Runtime footprint computation + `audit` + the block-on-widening gate.**
   Static analysis over the typed AST; `audit`, `why`, `why-cap`; the gate
   enforcing §10 against the lock. (The differentiator, end-to-end, offline.)
3. **coven: the signed registry.** TUF roles, namespaces, provenance, server-side
   footprint enforcement, keyless signing, `verify`, and the **two-phase
   `publish` (stage) → `promote` (out-of-band, second-factor release)** lifecycle
   of §8.1.
4. **Build-time capability model (§7.1).** Sandboxed build actors reusing the
   per-actor `Linker`; the `BuildOut`/`Read`/`Env`/`Net`/`Exec` capability set;
   build-footprint computation; per-rune `[build.grants]`; the gate extended to
   the build axis; `build_inputs` hashing for reproducibility. (Design-complete
   here; implementation when consumer-side build exec is first genuinely needed.)
5. **Polish & ecosystem.** Yank/audit advisories, `vendor`, std-as-runes
   decision, WIT-world forward-compat.

---

## 15. Implementation status

A working implementation lives in `src/pm/` (folded into the `witchy` binary)
with unit tests per module and `tests/e2e.rs` driving the real CLI through the
full lifecycle. What is built vs. modelled-for-later:

| Area | Status |
|---|---|
| Footprint engine (§4) | **Built.** `src/pm/footprint.rs` — static runtime+build footprint from the typed AST, transitive taint through user types. |
| Block-on-widening gate (§10) | **Built.** `src/pm/gate.rs` + enforced in `add`/`update`; blocks silent widening, `--allow-cap`/`--allow-build-cap` to consent. |
| Manifest / lockfile / semver / store (§5,6,7) | **Built.** `witchy.toml`, `witchy.lock` (pins hash+footprint+provenance), content-addressed store, PubGrub-lite resolution. |
| Two-phase publish (§8.1) | **Built (local).** stage → second-factor `promote`, immutability, separation of duties, server-side footprint recomputation. |
| Determinism tiering (§7.2) | **Built.** computed class surfaced by `audit`. |
| Build-grant enforcement (§7.1) | **Built (enforced; grant ⊇ demand).** Build *capability types* don't exist in the language yet, so build footprints are empty in practice — the machinery is ready for when they land. |
| CLI (§11) | **Built.** new/init/add/build/run/update/audit/why/why-cap/publish/promote/yank/list/verify/vendor. Never executes dependency code. |
| **Cryptographic record signing** (§8 targets role) | **Built.** `src/pm/keys.rs` — every registry record is **Ed25519-signed** by the registry root key; `fetch`/`build`/`verify` reject any record whose signature fails (catches metadata tampering that content-hashing alone would miss). The client **pins the key fingerprint (TOFU)** in `witchy.lock` and refuses to build if the registry's key changes. |
| **Networked registry server** (§8) | **Built.** `witchy coven-serve` (`src/pm/server.rs`, tiny_http) serves a JSON wire protocol; `src/pm/remote.rs` is the zero-trust HTTP client (verifies every record signature + source hash). `COVEN_URL` switches the CLI from the local model to a remote server. |
| **TUF snapshot + timestamp roles** (§8) | **Built.** `src/pm/tuf.rs` — the server regenerates/re-signs a version-numbered snapshot + a short-lived timestamp on every mutation; the client verifies the full chain (signatures, freshness ⇒ no freeze, version ≥ pinned ⇒ no rollback, per-record snapshot consistency). The snapshot version is pinned in `witchy.lock`. |
| **Trusted Publishing — keyless OIDC** (§8, §12) | **Built.** No long-lived API tokens exist. `src/pm/trusted.rs`: short-lived identity tokens (a JWT stand-in) from trusted issuers (a JWKS stand-in) carry CI/human claims; the server verifies them and matches a per-namespace **trust policy** (first trusted publish TOFU-binds issuer + `repository` + `workflow_ref`; later publishes must match). The publisher identity and a signed SLSA-style **provenance attestation** are derived from the *verified* claims, and **separation of duties** is enforced (the human who promotes ≠ the machine that staged). Bearer auth is rejected outright. `coven-gen-issuer`/`coven-mint-token` model the IdP/CI side. |
| Remaining registry refinements | **Modelled, not built.** A live-IdP adapter (real RS256/ES256 JWT parsing + JWKS fetched over https, e.g. GitHub Actions / Sigstore Fulcio+Rekor) in place of the Ed25519 stand-in, and full TUF **key separation** (per-namespace *delegated signing* keys — today all roles are signed by the one registry root key). |
| Sandboxed **build actor execution** (§7.1) | **Modelled, not built.** Awaits Build* capability types in the language; the footprint/grant/gate plumbing already accounts for it. |

The invariant holds today: **no dependency code is ever executed during
resolve/install/build** — only `witchy run` executes the user's own program.

Each phase preserves the invariant that **no rune code ever runs with ambient
authority** — build steps, when present, run only as sandboxed actors holding
exactly the build-time capabilities the user explicitly granted, pinned in the
lock and blocked from widening silently.
