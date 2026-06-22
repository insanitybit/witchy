---
rfc: 0004
title: Self-hosting the `witchy` CLI over a `witchyc` compiler
status: implemented
created: 2026-06-21
tracking: |
  Shipped 2026-06-22 (`src/pm` deleted, commit eae5276). The package manager
  (`projects/pm`) and registry (`projects/coven`) are self-hosted in witchy and
  drive the compiler via the `Exec` capability. One divergence from this proposal:
  it landed as a SINGLE binary that embeds the witchy front-end (loaded via the
  `witchy pm` / top-level and `coven-serve` bootstraps) rather than two separate
  `witchy` + `witchyc` binaries — the split is logical (TCB vs. front-end), not
  packaged. The trusted-publishing IdP minting (`coven-gen-issuer`/`coven-mint-token`)
  remains a Rust test helper in `src/idp.rs` (per §7).
---

# RFC-0004: Self-hosting the `witchy` CLI over a `witchyc` compiler

## Summary

Split today's single Rust binary into two layers:

- **`witchyc`** — the compiler and runtime, in **Rust**. Lex → parse → link →
  typecheck → codegen → WASM, the runtime VM, and the capability host functions.
  This is the irreducible trusted computing base (TCB).
- **`witchy`** — the user-facing CLI (package management *and* build
  orchestration), written in **witchy**. Everything a user types `witchy <verb>`
  for — `new/init/add/update/build/run/publish/promote/audit/…` — is a witchy
  program that drives `witchyc`.

`witchy` invokes `witchyc` through a single new, tightly-constrained runtime
capability, **`Exec`** (the runtime analog of the existing build-time
`BuildExec`), exposed to the front-end as a module-defined `capability Compiler
from (Exec, Dir[Read])` brand. The Rust package manager (`src/pm/`, ~6k lines) is **deleted**;
its logic moves into the witchy front-end and the existing self-hosted registry
(`projects/coven`). The only static-analysis piece that stays in Rust is the
capability-footprint engine, because it needs the typed AST — it is re-exposed as
`witchyc footprint`/`diff`.

The conceptual ancestor is the compiler/front-end split (a compiler primitive
plus a workflow tool that shells to it), but here the front-end is written in the
*target* language — this is a self-hosted toolchain, not a native one.

## Motivation

witchy's whole pitch is that authority is a typed, confined, legible value and
that the trusted native surface is kept as small as possible. Two things
undercut that today:

1. **The tool that wields the most supply-chain authority is opaque native
   code.** `witchy add`/`publish`/`build` are ~6k lines of Rust in `src/pm/`.
   The capability story ("a dependency can do nothing you didn't hand it") is far
   more convincing when the package manager *itself* is a witchy program with a
   visible, gated footprint — running as, in the words of the package-manager RFC
   §9, "an actor whose only capability is a `Net` attenuated to the coven host."
   This RFC makes that real and extends it: the front-end's footprint is exactly
   `Console, Dir, Net, Clock, Secret, Exec` — auditable like any rune.

2. **There are already two implementations, and nothing has shipped.** A complete
   Rust PM (`src/pm/`) sits beside a partial pure-witchy port (`projects/pm`
   client, `projects/coven` server). With no external consumers, the redundancy
   is pure cost: every feature is written twice or drifts, and the language never
   proves it can host its own tooling. Because nothing is shipped, the rewrite is
   free — there is no compatibility surface to preserve (see
   `feedback`/break-don't-deprecate).

**What goes wrong if we don't:** the Rust PM ossifies as a second source of
truth, dogfooding stays perpetually 80% done, and witchy never demonstrates the
one thing a capability-secure language most wants to demonstrate — that its own
build-and-distribute tooling can run under the same confinement it imposes on
everyone else.

## Design

### 1. The two layers

| Layer | Language | Responsibility |
|---|---|---|
| **`witchyc`** | Rust | parse, link, typecheck, derive/comptime/generators, optimize, codegen→WASM, the runtime VM, host functions (incl. the new `Exec`), `fmt`, `doc`, `lsp`, and the **footprint engine** (static analysis over the typed AST). |
| **`witchy`** | witchy | manifest + lockfile + store + resolve + semver, the capability **gate**, the registry **client** (`add/publish/promote/yank/verify/vendor`), and **build/run orchestration** — all by driving `witchyc`. |

The seam between them is the lockfile and the `witchyc` CLI. `witchy` decides
*what* to compile/run and *with which grants*; `witchyc` does the compiling and
running. This is exactly the split that exists informally today — `witchy run
foo.witchy` already lexes/typechecks/runs a module with host caps minted from
`--dir`/`--net` flags — only now the *decision layer* moves into witchy.

### 2. The `Exec` capability (the one new runtime primitive)

`Exec` is a runtime capability, the runtime analog of the existing build-time
`BuildExec` (see `rfcs/build-time-execution-plan.md`). It is the **only** new
language/runtime surface this RFC introduces.

**`Exec` is unparameterized; `Dir[Read]` names and confines the executable.**
Rather than baking a path allow-list into the `Exec` value, execution composes
with the existing directory-confinement machinery: to run a program you must
present **both** an `Exec` capability (the right to spawn a native process) and a
`Dir[Read]` through which the executable is named — exactly as a file read names
its target by `(dir, path)`. The invariant is **"you can only execute a file you
can read."** No bare absolute paths, no PATH search, no shell. The host mints
`Exec` only for `main` (a new `main` parameter kind); it carries no parameters
and never widens.

```
// the right to execute + a Dir that both NAMES and CONFINES the binary
fn main(console: Console, exec: Exec, bin: Dir[Read]):
    ...
```

**Operation.** A single buffered call; argv is passed as a list, never a shell
string, so there is no command-injection surface:

```
exec.run(dir: Dir[Read], path: String, args: List(String), stdin: String)
    -> ExecResult
// ExecResult { code: Int, out: String, err: String }
// `path` resolves within `dir` (same rules as `read(dir, path)`); escaping the
// Dir subtree, or calling without an Exec right, is a loud runtime error.
```

Confinement comes entirely from `Dir`: narrow the `Dir[Read]` subtree to narrow
what is executable, and grant a `Dir` containing exactly one file when you want
exact-binary confinement. This reuses one well-understood mechanism instead of
inventing a second allow-list, and keeps `Exec` itself a simple, unparameterized
capability *kind* in the footprint. (Streaming I/O and an env-passing variant are
deliberately deferred; v1 is buffered stdin → captured stdout/stderr/exit-code.)

**Parity.** `exec` is one host function implemented once and linked into *both*
the interpreter and the WASM VM (like the `Dir`/`Net` host functions), so both
backends observe identical behavior for a given run. Like `Net`/`Clock`, `exec`
is an effect and is **not** promised deterministic across runs — only identical
across backends within a run. A differential test in `src/example_tests.rs`
pins this.

**Footprint & gating.** `Exec` is a capability *kind* on the runtime axis. It
appears in `witchy caps`, and the block-on-widening gate refuses any rune that
newly demands it without an explicit `--allow-cap Exec`. This is the runtime
mirror of how `BuildExec` is already gated on the build axis.

**Security posture (the load-bearing part).** `Exec` is the single most
dangerous capability in the language — it runs native code and is, by
definition, a WASM-sandbox escape. Containment rests on three things, all
non-negotiable: (a) **you can only exec what you can read** — the binary is named
through a confined `Dir[Read]`, so reachability is bounded by directory
authority; (b) the call is **argv-only, never a shell string**; and (c) the
**footprint gate** makes any rune's `Exec` demand a loud, blocked,
explicitly-approved event. The standard library exposes **no** ambient
`Exec`-using helpers. In practice almost nothing should hold `Exec`; the witchy
front-end holds `Exec` plus a `Dir[Read]` to the toolchain `bin`, and that is the
intended near-sole use.

### 3. `capability Compiler from (Exec, Dir[Read])` — the front-end's branded handle

The front-end never touches raw `Exec` in its body. Its compiler-driver module
declares a sealed brand (per `rfcs/0002-user-definable-capabilities.md`) that
**bundles the execute right with a `Dir[Read]` rooted where `witchyc` lives**,
and exposes a typed surface instead of arbitrary exec:

```
capability Compiler from (Exec, Dir[Read])

// The ONLY way to obtain a Compiler — sealed to this module.
pub fn open(exec: Exec, bin: Dir[Read]) -> Compiler:
    Compiler(exec, bin)

pub fn compile(c: Compiler, plan: BuildPlan) -> Result(Wasm, String): ...
pub fn run(c: Compiler, plan: RunPlan) -> Result(Int, String): ...
pub fn check(c: Compiler, plan: BuildPlan) -> Result(Nil, String): ...
pub fn format(c: Compiler, src: String) -> Result(String, String): ...
pub fn footprint(c: Compiler, plan: BuildPlan) -> Result(Footprint, String): ...
// internally: exec.run(c.bin, "witchyc", …)
```

Bundling "may execute" (`Exec`) with "where `witchyc` is" (`Dir[Read]`) yields a
single "may run the compiler" handle. Because the brand refines `(Exec,
Dir[Read])`, the footprint **sees through** it — `witchy caps` reports `Exec,
Dir[Read] (refined: Compiler)` — so the friendly name cannot launder authority.
The brand is sugar over those two caps, but it buys a narrow typed surface and a
clean dogfood of user-definable capabilities.

### 4. The `witchyc` CLI / wire protocol

`witchyc` grows a small, stable command surface. Inputs are paths (the front-end
has already materialized resolved sources from the content-addressed store);
structured outputs (footprint, diff) are JSON on stdout; everything else is exit
code + stdout/stderr.

```
witchyc compile <entry> [--dep name=path]... [--out <wasm>]
witchyc run     <entry> [--dep name=path]... [--dir name=path]... \
                        [--net host:port]... [--secret ...]... [-- <args>...]
witchyc check   <entry> [--dep name=path]...
witchyc fmt     <file>
witchyc footprint <entry> [--dep name=path]...     # JSON: runtime + build axes
witchyc diff    <old-entry> <new-entry>            # JSON: widening delta
witchyc doc     <files>...
witchyc lsp                                         # stays a witchyc subcommand
```

`witchyc run` is precisely what `witchy <file>` does today: it mints the user
program's host caps from `--dir`/`--net`/`--secret` flags and runs the module.
The change is *who decides the flags*: the witchy front-end computes them from
the user's `main` signature plus the invoking command line, then calls `witchyc
run`. **No live capability value is forwarded** across the boundary — the trusted
`witchyc` subprocess re-derives host caps from grant *descriptions*, exactly as
`main.rs` does now. This is what makes the subprocess model strictly simpler than
an in-process compiler capability (see Alternatives).

### 5. Distribution & bootstrapping

One native executable, `witchyc`, ships. The toolchain **embeds the `witchy`
front-end source** (via `include_str!`, the same mechanism std uses today). The
`witchy` command is `witchyc` launching that embedded program:

```
$ witchy add acme/http@^1.2
  └─ witchyc launches the embedded front-end with:
       Console, Dir(cwd), Net, Clock, Secret, Exec + Dir[Read](bin), argv
  └─ front-end resolves, gates, fetches, writes witchy.lock        (witchy)

$ witchy run
  └─ front-end reads witchy.lock, gathers store sources            (witchy)
  └─ front-end: compiler.run(plan, grants)                          (witchy)
       └─ exec witchyc run <entry> --dep … --dir …                 (→ Rust)
```

The launcher resolves its own executable's directory and grants the front-end
`Exec` plus a `Dir[Read]` rooted there, so it can name and run `witchyc`; the
recursion is self-contained — one binary on disk, acting as the compiler when
given a `witchyc` subcommand and as the launcher otherwise.
"Separate `witchyc` binary vs. one binary with a launcher mode" is an install
detail, not a model decision.

**Trust.** The embedded front-end is part of the signed toolchain release, so it
is not an untrusted rune — but it still runs **capability-confined**, holding
only the caps above. Even our own tool obeys the model.

### 6. The partition of `src/`

- **Stays in `witchyc` (Rust):** `lexer`, `parser`, `ast`, `linker`, `typeck`,
  `traits`, `derive`, `comptime`, `generators`, `analysis`, `optimize`,
  `codegen`, `wir*`, `interpreter`, `runtime`, `value`, `native` (+ the new
  `exec` host fn), `fmt`, `doc`, `lsp`, and **`pm/footprint.rs`** (migrated out of
  `pm/` into the compiler proper as the backing of `witchyc footprint`). `main.rs`
  shrinks to: `witchyc` subcommand dispatch + the front-end launcher.
- **Moves into the `witchy` front-end (witchy):** `manifest` (on a new
  `std/toml`), `lockfile`, `store`, `resolve`, `semver`, `gate`, `registry`/
  `remote`/`wire`/`http` (the client), `keys`/`tuf`/`trusted` (Ed25519 + canonical
  JSON, already mirrored in `projects/coven`), and `cli` (the verbs).
- **Deleted:** all of `src/pm/` except the footprint engine noted above.
- **Server:** `projects/coven` already implements publish/promote/yank/fetch/
  sign/TUF and is cross-verified against the Rust verifiers; it stays and is the
  registry.

### 7. New stdlib

- **`std/toml`** — parse + serialize the manifest/lock subset
  (`[rune]`/`[capabilities]`/`[dependencies]`/`[build.grants]`, basic tables,
  strings, arrays, inline tables). `witchy.toml`/`witchy.lock` formats are
  unchanged.
- **hex codec** — encode/decode for key/hash handling (add to `std/encoding` if
  not already present; the Rust `keys.rs` hand-rolls it).
- **Key *generation*** has no witchy intrinsic and is *not* added: coven already
  receives its signing key as an injected `Secret` (`secrets.require("signing")`),
  and the test issuer/token tooling can stay `witchyc` test helpers.

### 8. Implementation plan (phased, each independently landable)

0. **Foundations (pure witchy):** `std/toml` + hex codec. No wiring.
1. **The keystone — `Exec` + the `witchyc` boundary:** add the constrained `Exec`
   host capability (both backends, narrowing, footprint, gate); give `witchyc` the
   `compile`/`run`/`check`/`fmt`/`footprint`/`diff` CLI. **Spike:** a witchy
   program holding `Exec` + a `Dir[Read]` to `witchyc` execs it to compile-and-run
   a trivial program and confirms the subprocess mints `Console`/`Dir` from grant
   flags. If that round-trips, the model holds.
2. **Front-end supply-chain surface (witchy):** port `resolve`/`semver`/`gate`/
   registry-client; fold `publish`/`promote`/`yank` in from
   `projects/coven/coven_client`; add `update/tree/outdated/why/why-cap/vendor/
   list/init/new`.
3. **Move build/run/check/fmt/test/doc onto the front-end** via the `Compiler`
   brand.
4. **Bootstrap + cutover:** embed the front-end, re-point `main.rs` dispatch,
   **delete `src/pm/`**, port `tests/e2e.rs` to drive the witchy CLI.
5. **Coven confirm:** verify the server runs under the new model; defer real-OIDC/
   JWKS and delegated TUF keys (unbuilt in Rust too — not blockers).

Phases 0 and 1 are independent; 2–3 depend on 1; 4 depends on 2–3.

## Alternatives

- **In-process `Compiler` host capability** (an earlier pick). The launcher
  injects a live `Compiler` cap; `build`/`run` call it in-process with no
  subprocess. Rejected: it requires a bespoke *built-in* capability and, for
  `run`, forwarding live host caps as values *through* a host function into a
  freshly-run module — novel and risky. `Exec` is more general, reuses the proven
  `BuildExec` pattern, keeps the built-in surface minimal, and sidesteps
  cap-forwarding entirely (grants cross as descriptions, not values).
- **Execute as a `Dir` right (`Dir[Exec]`) instead of a separate `Exec`
  capability.** Fold execution into directory rights, so `exec(dir, path)` needs
  `Dir[Exec]` and there is no top-level `Exec`. Tidy, but it hides "may spawn a
  native process" — the single most dangerous authority — inside a `Dir` right
  rather than surfacing it as its own footprint *kind*. Rejected: keep `Exec` a
  distinct, loud capability; let `Dir[Read]` only *name and confine* the target.
- **Standalone programs, no `witchy` integration.** Drop the subcommands; users
  run `witchyc run projects/pm/...` directly. Rejected: loses the integrated UX
  and manifest-relative ergonomics that make a package manager usable.
- **Keep the hybrid (Rust PM + witchy port).** Rejected: permanent redundancy,
  two sources of truth, dogfooding never completed.
- **Do nothing (Rust PM only).** Rejected: the language never hosts its own
  tooling; the capability-purity argument stays theoretical.
- **Switch manifests to JSON** instead of writing `std/toml`. Considered (witchy
  already has `std/json`), rejected to preserve `witchy.toml` ergonomics; the
  manifest subset is small enough that `std/toml` is modest.

## Drawbacks

- **`Exec` is a powerful, permanent new primitive** — a literal native escape.
  Its safety rests on the read-gated (`Dir[Read]`) / argv-only / footprint-gate
  discipline, which must be guarded forever and never relaxed into a general
  shell-out convenience. Note the confinement is *directory-coarse*: an `Exec`
  holder can run anything readable in the granted subtree, so exact-binary
  confinement requires granting a `Dir` that contains only that file.
- **Runtime `Exec` and build-time `BuildExec` now confine differently** — `Exec`
  by `Dir[Read]` subtree, `BuildExec` by a named-tool allow-list. They could be
  reconciled later; this RFC does not force it.
- **Subprocess overhead** per `build`/`run` (process spawn + path-based I/O).
  Negligible for a CLI, but nonzero; a long-lived `witchyc` server mode is a
  possible later optimization.
- **A new compatibility surface:** the `witchyc` CLI/wire protocol must be
  versioned and kept stable across the `witchy`↔`witchyc` boundary.
- **The partition isn't perfectly clean:** the footprint engine must stay in the
  compiler (it needs the typed AST), so `witchyc` keeps a faintly
  "package-manager-ish" responsibility (`witchyc footprint`/`diff`).
- **Bootstrapping coupling:** the toolchain now ships an embedded witchy program;
  the `witchyc` it ships with must always be able to build that front-end
  source — a chicken-and-egg to keep green in CI.
- **More install-time moving parts** (a compiler plus a launcher/shim).

## Open questions

- **Single-file capability granularity (`File` / `Fs`) — deferred.** Today `Dir`
  is always a *subtree*: `subdir` narrows to a sub-directory and files are named
  by string within a held `Dir`, so there is **no** capability for exactly one
  file. Exact-binary `Exec` confinement therefore relies on granting a `Dir`
  whose subtree contains only the target (directory-coarse; see Drawbacks). The
  clean fix is a `File` capability minted from a `Dir`
  (`file(dir, name) -> File[...]` — a further narrowing in the existing model),
  or, more sweepingly, reworking the filesystem capability into an `Fs` spanning
  both directory and file granularity. Both are broader than this RFC (they touch
  all `Dir` use, not just `Exec`) and are deferred to a future RFC; `Exec`-with-
  `Dir[Read]` is the motivating use that will most want single-file precision.

## Prior art

- The compiler-primitive / workflow-front-end split in mainstream toolchains —
  but those front-ends are written in the host's implementation language; this
  RFC writes the front-end in the *target* language (a self-hosted toolchain).
- `rfcs/build-time-execution-plan.md` — `BuildExec`, the build-time exec
  capability `Exec` is the runtime analog of.
- `rfcs/0002-user-definable-capabilities.md` — the `capability X from U` sealed
  brand used for `Compiler from (Exec, Dir[Read])`, and the "footprint sees
  through" rule.
- `spec/capabilities.md` — `Dir`-subtree confinement and rights-parameterization
  (`Dir[Read]`), reused to name and bound what `Exec` may run instead of a
  bespoke allow-list.
- `rfcs/package-manager.md` §9 — the original "PM as a `Net`-confined witchy
  program" intent this RFC fulfills; §8/§8.1 (coven, two-phase publish) are
  unchanged and now hosted entirely in witchy.
- cap-std / capability-based process execution generally.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
