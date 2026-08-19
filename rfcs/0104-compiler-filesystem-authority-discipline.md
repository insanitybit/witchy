---
rfc: 0104
title: "Compiler filesystem-authority discipline: cap-std and lint-gated ambient authority"
status: deferred
created: 2026-07-24
tracking: >
  Deferred 2026-08-19. No implementation cut or owner is currently scheduled.
  Revisit when a compiler-wide ambient-authority audit is staffed and the
  disallowed-method baseline can be maintained as a tracked migration ledger.
predecessors:
  - "[0103](0103-derived-platform-confinement.md) (derived confinement — adopted cap-std for the Dir capability; this RFC extends the same discipline to the compiler's own trusted filesystem access)"
  - "[0002](0002-user-definable-capabilities.md) (the capability doctrine the compiler's implementation should mirror)"
related:
  - "[0038](0038-grantable-user-capabilities.md) (grant documents / footprint — the language-level analog of the ambient-authority ledger this RFC introduces)"
  - "[0092](0092-trusted-application-executables.md) (trusted-exe — a native fs consumer this discipline covers)"
---

# RFC-0104: Compiler filesystem-authority discipline — cap-std and lint-gated ambient authority

> Provisional syntax. Code blocks here are intentionally **not** tagged
> `witchy` so the doc-examples test does not try to compile them.

## Summary

witchy is a capability-secure language; its Rust implementation is not. The
compiler, CLI, LSP, and native tooling reach the filesystem through ambient
`std::fs` — invisible, unaudited authority. This RFC makes the implementation
practice the discipline it enforces on programs:

- **Native Rust filesystem access goes through `cap-std`.** A directory
  handle (`cap_std::fs::Dir`) is a confined capability; operations are relative
  to it and race-free. This is already how the `Dir` *language* capability is
  backed (RFC-0103, `confine.rs`) — this RFC extends the same crate and pattern
  to the compiler's own trusted access.
- **Ambient authority becomes a named, grep-able, lint-gated exception.**
  Reaching outside a handle requires `cap_std::ambient_authority()`, funneled
  through one `ambient` module where each call site is justified in a comment.
  A `clippy.toml` `disallowed-methods` gate bans raw `std::fs::{read, write,
  File::open, …}`, so ambient authority cannot re-enter silently.
- **The value is auditability and dogfooding, not new enforcement.** The
  compiler is trusted; nothing here sandboxes it. What changes is that "where
  does the compiler touch the filesystem, and where does it do so ambiently?"
  becomes a `grep` and a lint instead of an unanswerable question — and the
  migration surfaces latent over-authority (a loader that reads more than its
  project root) as a side effect.

Scope is the **native Rust code** (the compiler workspace, the `witchy`
binary, `editors/zed`). The wasm32 playground build keeps `std::fs` behind
`cfg` (cap-std is native-only); the witchy-language tooling (`projects/pm`,
`projects/coven`) already runs on witchy's own `Dir` capability and needs
nothing. The guarantee is bounded (§Non-goals): it covers *our direct* fs
surface, not third-party crates.

## Motivation

### A capability language with an ambient-authority compiler is incongruent

The whole thesis of witchy is that authority is visible in a signature and a
footprint. Yet `witchy caps foo.witchy` is computed by a compiler that itself
calls `std::fs::read_to_string` on anything it is pointed at, with no record of
where or why. We ask users to route every effect through a capability; the tool
making that argument should be able to answer the same question about itself.
This is dogfooding, and for a security language it is close to table stakes.

### Ambient authority is currently unauditable

There are ~450 `std::fs::*` call sites across ~45 native Rust files (the
dominant ops: `remove_dir_all`, `write`, `read_to_string`, `create_dir_all`).
Some are legitimately ambient (a user-given absolute path, `$PATH` executable
lookup, a temp dir, config discovery walking parent directories); most are
scoped to a directory the compiler controls (an artifact cache, a project
tree). Today the two are indistinguishable — every one is a bare `std::fs`
call. There is no way to enumerate "the compiler's ambient filesystem
authority" short of reading 45 files by hand.

### The pattern is already proven in-tree

RFC-0103 backed the `Dir` capability with `cap_std::fs::Dir`
(`crates/witchy-runtime/src/confine.rs`: `ConfinedDir { inner:
Arc<cap_std::fs::Dir> }`, `open_ambient_dir(…, ambient_authority())`, and a
`#[cfg(target_arch = "wasm32")]` stub for the browser build). cap-std,
cap-primitives, and cap-fs-ext are already in `Cargo.lock` (4.0.2, via
wasmtime's WASI layer — the same Bytecode Alliance authors as the wasm
sandbox that is the core of our TCB). So this RFC adds **no dependency and no
new pattern** — it generalizes an existing, working one.

### Migration is also discovery

Porting a trusted path to cap-std forces naming its root. That turns latent
over-authority into an explicit question: does the source loader confine to the
project directory, or `std::fs::read` wherever it is pointed? Does dependency
resolution stay within the rune, or wander? Under cap-std these become
`open_ambient_dir` calls a reviewer can challenge; under `std::fs` they are
invisible. The migration is a security audit that happens to leave the code
tidier.

## Doctrine

### Handles by default, ambient by exception

A `cap_std::fs::Dir` is a confined capability: you open it once (rooted at a
directory the operation legitimately owns) and every read/write/create is
relative to it and cannot escape. Code that operates within a known root uses a
handle. Code that genuinely needs unrestricted access — because the authority
*is* ambient (the user handed us an absolute path; we resolve `$PATH`; we make
a temp dir; we walk up to find a config file) — obtains it through
`ambient_authority()`, and that is the only way to do so.

### One `ambient` module = the authority ledger

All `ambient_authority()` acquisition funnels through a single module (e.g.
`src/ambient.rs` for the binary, a shared helper for the workspace). Each
function there wraps one legitimate ambient need and documents *why* it is
ambient:

```rust
/// Open a directory the USER named on the command line (an absolute path we
/// were explicitly pointed at). Ambient by nature: the user's authority, not
/// the compiler's.
pub fn user_named_dir(path: &Path) -> io::Result<Dir> { … }

/// The per-user artifact cache under $XDG_CACHE_HOME. Ambient because its
/// location is outside any project root.
pub fn cache_dir() -> io::Result<Dir> { … }
```

The module *is* the answer to "what ambient filesystem authority does the
compiler take, and why" — a file, reviewable in one place, instead of a grep
across the tree. It is the Rust-side analog of a grant document (RFC-0013): an
explicit, enumerated statement of ambient authority.

### The lint is the ratchet

A `clippy.toml` `disallowed-methods` entry bans the raw `std::fs` free
functions and `std::fs::File::{open, create, create_new}`, with a message
pointing at cap-std / the `ambient` module. New code cannot reach the
filesystem ambiently without either a handle or a marked `ambient` call that
trips review. Existing sites are baselined (an allow-list, or per-line
`#[allow(clippy::disallowed_methods)]` with a `// AMBIENT:` justification) and
removed cluster by cluster as they migrate. The gate ships **non-blocking
first** (warn + full baseline) so it never blocks unrelated work, and tightens
as the surface shrinks.

## Scope and non-goals

**In scope:** native Rust — the compiler stage crates, the `witchy` binary
(`src/`: CLI, commands, LSP, source loading), and `editors/zed`.

**Out of scope, by necessity or design:**

- **The wasm32 playground build.** cap-std does not build for
  `wasm32-unknown-unknown`; those paths keep `std::fs` behind `cfg`, exactly as
  `confine.rs` already does (`cap_std::fs::File` on native, an `unsupported()`
  stub on wasm). This is not a gap: the browser interpreter uses the in-memory
  `Mock` Dir and never touches a real filesystem.
- **The witchy-language tooling.** `projects/{pm, coven, coven-web}` are
  written in witchy (0 `.rs`) and already run on witchy's `Dir` capability —
  they dogfood by construction and need nothing here.
- **The `Dir`/`File` language capability enforcement.** Done in RFC-0103; this
  RFC does not touch it.

**Non-goals (the guarantee is bounded, and says so):**

- **Not airtight.** Third-party crates (`git2`, `tar`/`zip`, `tempfile`,
  `serde` reading a path, the zed extension's deps) perform ambient `std::fs`
  internally; neither cap-std nor the lint reaches into them. The gate covers
  **our direct fs surface** — where our own ambient-authority bugs live — not a
  total accounting. Overselling this as a closed guarantee would be exactly the
  kind of security-theater witchy exists to avoid.
- **Not sandboxing the compiler.** The compiler is part of the TCB (RFC-0103
  §trust boundaries); this changes *auditability*, not the trust model.
- **Not a behavior change.** A correctly migrated trusted path reads and writes
  the same files as before; this is refactoring, parity-neutral by
  construction.

## Test code

The gate applies to production (`not(test)`) code. Test harnesses legitimately
create and tear down scratch directories with ambient authority (`env::temp_dir`
+ `remove_dir_all`), are not part of the shipped surface, and pinning them
through handles buys nothing. `#[cfg(test)]` blocks and the `tests/` tree are
exempt from the lint (a shared `test_scratch()` helper may still be offered for
ergonomics). This exemption can ratchet later if desired; it is not load-bearing
for the RFC's value, which is about shipped ambient authority.

## Rejected alternatives

- **Convention only ("prefer cap-std").** No teeth; drifts immediately. The
  lint is what turns a grep into a guarantee-about-new-code.
- **A hand-rolled fs wrapper instead of cap-std.** Reinvents an audited crate
  already in our lock and already trusted for the wasm sandbox and the `Dir`
  capability. cap-std's `ambient_authority()` token is precisely the "make
  ambient explicit and searchable" primitive this RFC wants; building our own
  would be strictly worse.
- **Forbidding ambient authority entirely.** Impossible and wrong: a compiler
  *must* read a user-given absolute path, resolve `$PATH`, and make temp dirs.
  The goal is to *mark and enumerate* ambient authority, not eliminate it.
- **Applying it to the wasm target.** cap-std does not compile there and those
  paths never touch a real filesystem; the `cfg` split is correct.
- **Applying it to the witchy-language tooling.** pm/coven are already
  capability-secure witchy programs; there is nothing to migrate.
- **A blanket `deny` from day one.** Would block unrelated work behind a
  450-site migration. Non-blocking baseline + ratchet lands the discipline
  without a flag day.

## Prior art

- **cap-std / Capsicum.** The capability-oriented filesystem model this adopts;
  `ambient_authority()` is cap-std's explicit-ambient-authority design, built
  for exactly this use.
- **`confine.rs` (RFC-0103).** The in-tree proof: `ConfinedDir` over
  `Arc<cap_std::fs::Dir>`, `open_ambient_dir(…, ambient_authority())`, wasm
  `cfg` stub. This RFC generalizes that one working instance.
- **witchy's own capability model.** This is the Rust-side analog: authority is
  a value you hold (a `Dir`), ambient authority is a marked exception, and the
  `ambient` module is a grant document for the compiler itself.

## Future work

- **Extend to network authority.** The pinned-dial machinery (RFC-0020) and a
  cap-std-style `cap_net` treatment could give outbound network the same
  handle-vs-ambient discipline, unifying with RFC-0102's `Fetch`.
- **Environment and process authority.** `std::env::var` and
  `Command`/`$PATH` lookup are ambient authority too; the same `ambient` module
  and lint approach applies (`disallowed-methods` on `std::env::var`,
  `Command::new`), tracked separately.
- **An emitted ambient-authority report.** Since all ambient access funnels
  through one module, `witchy` could print its own ambient-authority footprint
  — the compiler auditing itself with the same vocabulary it gives programs.

## Implementation phases and evidence

1. **Gate + ledger, non-blocking.** Add `clippy.toml` `disallowed-methods` for
   the `std::fs` free functions and `File::{open,create,create_new}` (warn or
   deny-with-full-baseline); add the `ambient` module; convert the clearest
   handful of genuinely-ambient sites (user-path, cache dir, temp) to it as the
   worked example. Evidence: `cargo clippy` green with the baseline; the
   `ambient` module compiles and its call sites carry justifications.
2..N. **Migrate clusters, ratchet the baseline.** One branch per cluster,
   ordered by value: the source loader and dependency resolution first (they
   double as the over-authority audit), then the artifact/compile caches,
   package (tar/extract) I/O, trusted-exe emission, LSP file access, and
   `editors/zed`. Each branch removes its sites from the baseline. Evidence per
   branch: parity-neutral (same files touched — the existing suites adjudicate);
   the removed `std::fs` sites now go through a handle or a justified `ambient`
   call; the baseline strictly shrinks.
3. **Close-out.** When the baseline is empty for a crate, the lint becomes
   `deny` for that crate. Evidence: a `disallowed-methods` deny with no
   remaining `#[allow]`s.

Each phase lands through the serialized gate. Phases are individually
valuable and independently landable; the discipline is in force for new code
from phase 1.

## Compatibility

No behavior change and no dependency change (cap-std is already in the lock).
The lint is additive and baselined, so it never blocks existing work. wasm and
the witchy-language tooling are untouched. Migrated code reads and writes
exactly the files it did before — the existing test suites are the evidence.
