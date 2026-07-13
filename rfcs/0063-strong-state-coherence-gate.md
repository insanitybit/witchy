---
rfc: 0063
title: Strong-state coherence gate for a proud 0.1 release
status: accepted
created: 2026-07-04
superseded-by:
tracking: coherence gate accepted as the pre-tag bar; executed alongside RFC-0061; intrinsic-name and host-import authority catalog slices landed
---

# RFC-0063: Strong-state coherence gate for a proud 0.1 release

## Summary

RFC-0061 defines a checkable 0.1.0 gate: drain reviewed RFCs, close prime-directive
bugs above LOW, and finish launch hygiene. That is necessary but not sufficient for a
release the project can show proudly as a coherent security-oriented system. This RFC
adds a final **coherence gate**: the remaining truth-table duplication, boundary
inconsistencies, public-demo gaps, and release-tooling mismatches must be fixed or
explicitly accepted before the first public tag.

## Motivation

The current implementation is impressive, but several high-value promises are still
spread across multiple independent mechanisms:

- capability authority is represented in typeck, footprint analysis, grant docs, runtime
  import linking, precompiled-wasm import inference, and browser host shells;
- builtin/intrinsic operation semantics are repeated across typeck, interpreter, lowering,
  WIR helper registries, linker allowlists, and tests;
- the compiler architecture docs describe a clean WIR pipeline, while lowering still
  exposes WAT-era fallback vocabulary and migration scaffolding;
- the flagship browser/product surfaces talk in capability-safe terms, but some host
  boundaries still accept raw strings/commands without re-checking the policy;
- local release/concurrency scripts do not fully follow the target-dir guidance they now
  recommend.

A green `./scripts/check.sh --full` can coexist with these issues. A public release should
not hide them in prose; it should either fix them or declare exactly what remains research
preview.

## Design

### 1. Canonical security and bug ledger

Before tagging, the repo must have one canonical current-state security ledger:

- every open `security-eval` finding is fixed, downgraded with evidence, or explicitly
  accepted/deferred with a rationale;
- every user-visible or security-relevant gap from the final review is represented in
  `bugs/` or an RFC;
- README/SECURITY language matches the ledger. If Witchy is a research preview, say that
  cleanly; if it is presented as a security boundary, remove the "shouldn't be trusted"
  posture only after the ledger supports that claim.

### 2. One capability/import authority catalog

Create one machine-readable catalog for the host import surface. It should define, for each
`witchy.*` import:

- import name and ABI shape;
- authority class: capability-gated, authority-free infrastructure, toolchain-only, or
  browser-only;
- required capability kind and rights/verbs, if any;
- whether it is valid in source execution, precompiled `.wasm`, native host, browser host,
  build step, or compiler-introspection contexts.

`link_capability_imports`, precompiled `.wasm` classification, WIR prelude import metadata,
and browser host conformance tests should consume this catalog or be checked against it.

This directly closes drift bugs such as BUG-013 and prevents future import-name skew.

Implementation progress (2026-07-12): the WIR prelude ABI catalog now carries
the concrete authority family for every capability-bearing `witchy.*` import,
including multi-authority imports such as `net_listen_tls` (`Net.Listen` +
`Secret`). The precompiled `.wasm` runner consumes that catalog instead of
owning local name arrays for `Dir`, `Net`, `Secret`, `Clock`, `Rand`, `Env`,
`Exec`, and direct `File` grants. The committed `spec/wasm-abi.md` table is
generated from the same metadata, and catalog tests require every
capability-authority import to name a concrete authority family.

### 3. One intrinsic/operation catalog

Create a shared compiler intrinsic registry for builtin and std-backed operations. The
registry should carry:

- name and arity;
- type signature and purity/effect class;
- interpreter/native implementation hook;
- lowering kind / WIR helper dependencies;
- capability effect, if any;
- diagnostic name.

The goal is not to force one implementation body. The goal is to stop repeating operation
identity and signatures across typeck, interpreter, lower/codegen, WIR helper selection,
and linker allowlists.

Implementation progress (2026-07-12): the first slice centralizes compiler/private
intrinsic identities and std-bridge owner allowlists in `witchy-syntax::intrinsics`.
Parser desugaring, formatter re-sugaring, linker privacy checks, type signatures,
interpreter dispatch, lowering, and runtime/native lookup now consume those shared names.
This deliberately does not complete the broader operation registry; arity, signature,
purity/effect, WIR helper, and diagnostic metadata still need to move into cataloged
facts before this RFC is fully implemented.

### 4. Typed compiler facts, not string or address shadows

The final compiler story should not depend on shadow encodings:

- method/trait dispatch should use structured type keys from typeck, not parsed type-name
  strings except for diagnostics;
- type facts should be stable across AST clones/rewrites, via stable node IDs or an
  annotated AST, not raw expression addresses;
- lowering should use explicit outcomes such as `Lowered`, `Unsupported`, and `Reject`
  instead of `Option<WirExpr>` plus comments about falling through to "legacy" behavior.

This is the work that makes RFC-0018's stage boundaries feel real in the code, not just
the docs.

### 5. Flagship product boundary hardening

The public demos should demonstrate the security model rather than approximate it:

- Glamour `UiFetch` method/prefix policies must be enforced when commands are created and
  re-checked by the host shell;
- host ports should return structured success/error values, not successful strings with an
  `error:` prefix;
- Coven Web must not turn failed credential operations into logged-in state;
- mutable WebAuthn/session state must not live under static asset roots;
- Coven package names and query construction need one URL-safe grammar and encoding helper;
- generated web assets need freshness verification or a clear "build artifact only" model.

### 6. Release tooling coherence

Local and CI release gates should match their labels:

- isolated `CARGO_TARGET_DIR` must work through `check.sh`, e2e, and playground builds;
- `just` recipes must point to live scripts;
- docs builds should fail, or explicitly opt out, when runnable compiler assets are missing;
- release artifacts should include a clear `--version`/commit identity, checksums, and a
  documented verification path.

Implementation progress (2026-07-13): `scripts/build-docs.sh` now fails before
assembling a bundle when the browser compiler is absent. Non-runnable render
smokes must opt out explicitly with `--allow-missing-compiler`; `just
docs-build` and CI continue to stage the compiler and use the strict path. The
real-book bundle test proves both the default rejection and the intentional
non-runnable opt-out.

## Alternatives

- **Do only RFC-0061.** This gives a green checkpoint, but the architecture can still feel
  patched together and the browser/security story can still have boundary mismatches.
- **Defer everything to post-0.1.** Acceptable only if README/CHANGELOG frame 0.1 as a
  research snapshot, not as something to trust or show as a polished security system.
- **Rewrite everything around registries first.** Too large. The incremental path is to
  introduce catalogs as read-only facts/tests first, then move consumers over one by one.

## Drawbacks

This adds a second release bar after the already-large RFC-0061 effort. It may delay the
first tag. The payoff is that the tag means more than "all tests passed": it means Witchy's
central claims are represented consistently in code, docs, tests, and demos.

## Prior art

No external prior art is required for this decision. It follows the repo's own RFC-0018
stage-boundary discipline, RFC-0058 harness-integrity work, RFC-0061 release gate, and the
capability model described in `spec/capabilities.md`.
