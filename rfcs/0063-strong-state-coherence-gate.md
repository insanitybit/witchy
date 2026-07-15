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
The next slice turns those identities into a representation-neutral catalog carrying
exact arity, a type-signature recipe, semantic effect class, capability effect, lowering
class, WIR helper dependencies, diagnostic name, and private callers. Type checking now
derives private intrinsic signatures from those recipes. The interpreter rejects arity
drift through the cataloged diagnostic contract when dispatch reaches a private builtin;
type checking checks the same arity through the cataloged signature. Coherence tests
reject source-placeholder signature drift, missing native hooks, source-function arity
drift, and references to absent static WIR helpers. This deliberately does not complete
the broader operation registry:
public builtins and std-backed operations still need catalog entries, and runtime/lowering
hook selection must move over consumer by consumer before this RFC is fully implemented.

The first public-family migration catalogs the compiler-introspection operations
(`compiler.footprint`, `compiler.diff`, `compiler.doc`, and the private typed-result
bridge). Their source placeholders, native registry hooks, result representation,
compiled lowering helpers, arity, toolchain effect, and diagnostics now resolve from or
are checked against the same catalog rows. This is the template for migrating the
remaining operation families without one monolithic registry rewrite.

The encoding family is the first selector-based host family to use the same model. Its
fourteen native operations now catalog their exact semantic signature, pure effect, shared
`encoding` WIR helper, numeric selector, and host input representation. Compiled lowering
and runtime host dispatch resolve the same selector row, result representation derives
from the catalog signature, and tests reject selector collisions, drift in the thirteen
`std/encoding.witchy` declarations, missing native hooks, or a missing WIR helper. The
host-only `encoding.utf8_lossy` operation has no source declaration; its explicit
`LossyUtf8Bytes` host input records the one representation bridge that cannot be inferred
from its native `String -> String` contract, and `$bytes_to_string` obtains selector 7 from
that catalog row rather than embedding it independently.

The string primitive family now catalogs all fifteen backend-crossing operations,
including exact semantic signatures, pure/no-capability effects, interpreter-versus-native
runtime ownership, and direct WIR helper dependencies. Type checking derives every string
primitive signature from those rows; placeholder suppression, interpreter dispatch,
native lookup, lowering identity, and compiled result-shape classification consume the
catalog family or its stable names instead of maintaining separate string-name sets.
Coherence tests compare the rows to `std/string.witchy`, require every declared helper to
exist in the WIR registry, and require every primitive placeholder to be suppressed. This
also closes the prior omission where `string.from_code` was intercepted by both backends
but its self-recursive source declaration was not classified as an intrinsic.

The math primitive family catalogs all three backend-crossing operations:
`math.to_float`, `math.to_int`, and `math.sqrt`. Their rows carry exact signatures,
pure/no-capability effects, interpreter ownership, and the direct `float_to_int` WIR
helper dependency; `math.to_float` and `math.sqrt` remain typed inline operations with no
helper. Type checking derives the three signatures from those rows, while interpreter
dispatch, lowering identity, analysis aliases, and compiled result-shape classification
consume the catalog family or its stable names. Coherence tests require source declarations
and helpers to exist, reject primitive-placeholder drift, and execute every row through the
interpreter-owned dispatch path.

The List primitive family catalogs its six backend-crossing operations: `list.length`,
`list.at`, `list.__push`, `list.__set_at`, `list.concat`, and
`list.__pop_extract`. Their rows carry generic signatures, no-capability effects,
interpreter ownership, and every baseline/optimized/view/bounds WIR helper dependency. The five
value transforms are pure; extraction is explicitly a write-back effect. Type checking
derives the generic signatures from those rows, compiled result-shape classification reads
the catalog predicates, and parser desugaring, aliases, interpreter dispatch, escape and
uniqueness analysis, and lowering consume stable catalog names. Specialized extraction
names resolve to the canonical operation row instead of bypassing its contract. Coherence
tests compare every row with `std/list.witchy` (including the `var` receiver), derive the
reverse set of primitive placeholders from source, execute exact operation results and
write-back dispatch, require every helper to exist, and require every self-recursive
primitive placeholder to be suppressed.

The Dict primitive family catalogs all thirteen backend-crossing operations, including
the fused `__insert_extract` and `__remove_extract` write-back operations omitted by the
older eleven-name suppression table. Generic key/value signatures, the eight `k: Eq`
requirements, unique-receiver declarations, pure versus write-back effects, dynamic
compound-key equality, and baseline/optimized WIR helper dependencies are explicit
catalog facts. Concrete catalog recipes now outrank linked source placeholders in type
checking for every catalog family; source remains the checked home for parameter
conventions and ownership qualifiers. Interpreter dispatch, result-shape classification,
place lowering, aliases, escape/uniqueness analysis, and WIR lowering consume stable
catalog identities or predicates. Coherence tests derive the reverse primitive set from
`std/dict.witchy`, compare signatures, bounds, conventions, and qualifiers, execute exact
runtime and write-back results, and reject missing helpers or unsuppressed placeholders.
The remaining public operation families are the next catalog migrations.

### 4. Typed compiler facts, not string or address shadows

The final compiler story should not depend on shadow encodings:

- method/trait dispatch should use structured type keys from typeck, not parsed type-name
  strings except for diagnostics;
- type facts should be stable across AST clones/rewrites, via stable node IDs or an
  annotated AST, not raw expression addresses;
- lowering should use explicit outcomes such as `Lowered`, `Unsupported`, and `Rejected`
  instead of `Option<WirExpr>` plus comments about falling through to "legacy" behavior.

This is the work that makes RFC-0018's stage boundaries feel real in the code, not just
the docs.

Implementation progress (2026-07-15): the general module-to-WIR boundaries now return
`LoweringOutcome`: `compile_module_binary`, the pre-optimization and optimized WIR
assemblers, and the build-step wrapper distinguish successful lowering, a checked valid
module that reaches an unimplemented capability-correct lowering, and a hard rejection.
Callers must supply the linked, lowered, type-checked module promised by the pipeline;
defensive tests may pass an unchecked `Module`, but its category is outside this contract.
The production encoder is wrapped by a fallible boundary, and every public output rejects
encoder invariant failures or wasm validation failures instead of returning malformed
compiler output as `Lowered`.
All direct callers match the three outcomes, and tests independently pin outcome
conversion, successful/rejected source modules, and malformed-WIR rejection. The internal
expression and statement builders still use `Option<WirExpr>`/`Option<WirSeq>` as their
local propagation mechanism; replacing those shadows with typed failure reasons is the
remaining lowering-outcome work rather than something this slice claims to complete.

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
