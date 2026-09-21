---
rfc: 0168
title: Split the toolchain into `witchyc` (compiler) and `witchy` (project driver)
status: proposed
created: 2026-09-21
amends: 0004
tracking:
---

# RFC-0168: Split the toolchain into `witchyc` and `witchy`

## Summary

Ship two programs with the division of labour Rust uses for `rustc` and
`cargo`:

- **`witchyc`** is the compiler. It takes one compilation unit and an explicit,
  already-resolved set of dependency inputs, and emits an admitted artifact plus
  diagnostics. It has no notion of a project, a manifest, a registry, a network,
  a lockfile, or a web framework.
- **`witchy`** is a thin project and package driver. It reads `witchy.toml`,
  resolves and verifies dependencies, invokes `witchyc` once per unit, runs the
  result, and owns every workflow that is not "turn this source into that
  artifact".

Everything that is not the compiler leaves the compiler's source tree and its
binary: the registry server, the package manager, and the glamour web
framework.

## Motivation

RFC-0004 proposed exactly this split and was marked implemented, but its own
tracking note records that it "landed as a SINGLE binary that embeds the witchy
front-end … the split is logical (TCB vs. front-end), not packaged." The logical
split has not held. Measured on `master` at `aef2ec1f3`:

**Consumer source is compiled into the compiler binary.** Eighteen files from
`projects/` are `include_str!`/`include_bytes!`'d into Rust code, through four
separate mechanisms:

| mechanism | where | what it embeds |
|---|---|---|
| bundled import namespace | `crates/witchy-syntax/src/linker.rs` `bundled_source` | `glamour`, `markdown`, `highlight` beside `std`, so any program may `import glamour` |
| compiler package-input path | `src/source/package_inputs/registry.rs` | grimoire `gm_core` and coven `coven_artifact`, linked as the verifier for vendored inputs |
| registry server | `src/commands/dispatch.rs` (`coven-serve`) | all eleven `projects/coven/src/*.witchy` |
| package manager | `src/commands/embedded_pm.rs` | grimoire and four coven modules |

**The code generator special-cases one application framework.**
`crates/witchy-lower/src/codegen/glamour_metadata.rs` is 4,746 lines of
glamour-specific lowering (islands, workers, templates, static evaluation) and
checks declarations are "owned by the toolchain Glamour package".
`codegen/assembly.rs` mentions glamour 208 times and
`witchy-types/src/pipeline/static_evaluation.rs` 30. The web commands built on
top are another 12,980 lines (`static_site.rs` 7,172, `web.rs` 3,973,
`dev.rs` 1,835).

This costs us in three concrete ways:

1. **Compiler work is slower.** Any edit to a consumer rebuilds the compiler,
   invalidates caches and needs a full gate. Build is about 80% of the merge
   gate's test stage (median 221s of build per gate over 59 measured gates),
   and the root `witchy` package's 20-odd test binaries dominate it. The
   compiler cannot be built or tested without the whole product.
2. **The compiler's correctness depends on application code.** Compiling a
   program with vendored dependencies runs grimoire and coven code as a
   verifier. A bug in the package manager can break compilation.
3. **Consumers cannot be dropped.** On 2026-09-21 glamour, coven, coven-web and
   grimoire were deprioritized. Their tests left the gate, but their source
   could not, because the compiler binary is built from it.

## Design

### `witchyc`

A new crate, `crates/witchyc`, producing the `witchyc` binary. It depends only
on the compiler crates: `witchy-syntax`, `witchy-types`, `witchy-lower`,
`witchy-wir` (without `native`), `witchy-cap-model`, `witchy-caps`. Those crates
already build without Wasmtime (it is a dev or optional dependency in every one
of them), so `witchyc` carries no runtime.

Its contract is deliberately `rustc`-shaped: every input is explicit on the
command line and nothing is discovered.

```sh
witchyc <entry.witchy>
    --crate-name <name>
    --extern <alias>=<path>[@<blake3>]...   # resolved dependency, with its content digest
    --emit wasm,wat,caps,diagnostics-json
    --out-dir <dir>
    [--mode check]                         # type-check and admit, emit nothing
```

- The standard library is the only bundled module set. `bundled_source`
  becomes `std_source`.
- `witchyc` verifies that each `--extern` input matches its digest and nothing
  more. Where a digest came from, which registry vouched for it, and whether a
  signature was valid are `witchy`'s questions.
- Output is an admitted artifact and, with `--emit caps`, the capability
  footprint as data, so `witchy` can enforce grants without re-deriving them.

Commands that are properties of one compilation unit stay with the compiler:
`check`, `compile`, `emit-wat`, `expand`, `caps`, `caps-diff`, `which`, `doc`.

### `witchy`

The existing root package becomes the driver. It owns everything that needs a
project, a manifest, a network, or a policy decision:

- `new`, `init`, `add`, `build`, `run`, `test`, `update`, `vendor`, `tree`,
  `audit`, `verify`, `publish`, `promote`, `yank`
- dependency resolution, lockfiles, registry access and signature and pin
  verification, today split between grimoire and `src/source/package_inputs`
- grants: `grants-check`, `grants-diff`, `sandbox`
- the runtime host for `run`, `test` and `sandbox` (Wasmtime plus capability
  host functions)

`witchy build` resolves and verifies the dependency graph, then invokes
`witchyc` once per unit with explicit `--extern` inputs, exactly as `cargo`
drives `rustc`. `witchy` never parses witchy source itself.

### What leaves both

- **The coven registry server** (`coven-serve`, `coven-gen-issuer`,
  `coven-issuer-jwks`) becomes its own program. It is an application written in
  witchy, run by `witchy run` with its grants, or packaged as its own binary for
  the deploy image.
- **glamour** stops being part of the toolchain. It is a normal package that
  applications depend on by path or registry, like the docs site already does
  (`projects/docs/witchy.toml`: `"glamour" = { path = "../glamour" }`). The
  web commands (`new --web`, `build --web`, `test --web`, `doctor --web`, `dev`)
  become an external subcommand, `witchy-web`, discovered on `PATH` the way
  `cargo` discovers `cargo-<name>`.

## Plan

Each phase leaves `master` releasable. The order puts the phases that make the
compiler's inner loop faster first and the phases that need product decisions
last.

**Phase 1: the `witchyc` boundary, with no behaviour change.**
Add `crates/witchyc` with the contract above, implemented on the existing
compiler crates. `witchy check` and `witchy compile` call it in-process through
the same library entry points. Move compiler-only tests to the compiler crates
so a compiler change can be gated on `witchyc` and its crates alone.
*Acceptance:* `cargo tree -p witchyc` has no `wasmtime`, `witchy-runtime`,
`witchy-interp` or web crate; `witchyc` builds and runs the RFC-0167 conformance
corpus; a batch touching only compiler crates builds no root test binary in the
gate.

**Phase 2: move the registry server out.**
Replace `witchy coven-serve` with a standalone launch of `projects/coven`, then
update `projects/coven/deploy`, `projects/coven-web/deploy`, `spec/local-registry.md`
and the e2e suite before deleting the dispatch branch. The live deploy image
runs `coven-serve` today, so the new launch path has to ship first.
*Acceptance:* no `projects/coven` file is embedded in any binary built from
the root package; the deploy image builds from the new path.

**Phase 3: move package verification into `witchy`.**
`witchy` resolves and verifies dependencies and hands `witchyc` digests. Delete
the grimoire and coven verifier from `src/source/package_inputs/registry.rs`.
This is the phase with real security content: vendored-package verification
changes owner, and RFC-0163's authority checks have to hold across the new
process boundary. It needs an adversarial review of the trust boundary.
*Acceptance:* no `projects/` source reaches the compiler's input path;
`witchyc` rejects a mismatched `--extern` digest; the existing vendor-replay
security tests pass against `witchy`.

**Phase 4: glamour becomes a package.**
Drop `glamour`, `markdown` and `highlight` from the bundled namespace, move the
web commands to `witchy-web`, and decide the fate of `glamour_metadata.rs`
(Open questions).
*Acceptance:* `bundled_source` is the standard library only; `witchyc` contains
no string `glamour`; the docs site builds through its path dependency.

**Phase 5: the package manager stops being embedded source.**
`witchy` loads grimoire as a prebuilt admitted artifact shipped with the
toolchain, or links it as a Rust crate, instead of compiling `include_str!`
source at run time.
*Acceptance:* zero `include_str!`/`include_bytes!` of `projects/` anywhere in
the toolchain.

## Alternatives

- **Keep one binary and enforce the split with lints.** This is what RFC-0004
  did, and the tree above is what it produced. A boundary that the build does not
  enforce erodes.
- **Keep `witchyc` as a library only, with no binary.** Cheaper, and enough for
  the build-time benefit. But the `rustc` contract, explicit inputs and no
  discovery, is also what makes the trust boundary in Phase 3 reviewable. A
  binary makes the contract real and lets `witchy` run the compiler confined,
  which RFC-0163's compiler worker already does for adversarial source.
- **Delete glamour and coven outright.** Faster, but it is a product decision
  rather than an architecture one, and the docs site is a glamour application.
  This RFC makes deletion possible without deciding it.

## Open questions

1. **Where does the runtime host live?** `cargo run` hands the built binary to
   the operating system. A witchy artifact needs the Wasmtime host and the
   capability host functions, which are part of the TCB. Proposal: `witchy`
   owns execution for now; revisit a separate `witchy-host` if the driver grows.
2. **What happens to `glamour_metadata.rs`?** Either generalize islands,
   workers and templates into a documented language mechanism with no
   framework's name on it, or delete it with `build --web`. Given the
   deprioritization, the default is to freeze it until Phase 4 and decide then.
3. **Does `witchy` stay self-hosted?** RFC-0004 wrote the package manager in
   witchy. `cargo` is written in Rust. Keeping grimoire in witchy is compatible
   with this RFC as long as it ships as an artifact rather than embedded source.
4. **Where do `fmt` and `lsp` go?** `rustfmt` and `rust-analyzer` are separate
   tools. Both are small enough to stay behind `witchy` for now.
5. **How is the `witchyc` command line versioned?** `witchy` and `witchyc` ship
   together, so the contract can be unstable at first, as `rustc`'s is.
