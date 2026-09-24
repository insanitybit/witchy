---
rfc: 0170
title: Cut `witchy-wir` loose from `witchy-types`
status: implemented
created: 2026-09-24
tracking:
---

# RFC-0170: Cut `witchy-wir` loose from `witchy-types`

## Summary

`witchy-wir` depends on `witchy-types` only to read a single checker product,
`CanonicalCheckedSource`, plus a few AST-level helpers that happen to live in
`witchy-types`. That one edge puts the two largest crates in the workspace in
series on every build, and it makes every typeck edit rebuild SafeWIR, the Wasm
encoder and the runtime.

This RFC moves the source-level contract that SafeWIR actually consumes into a
small crate, `witchy-canonical-source`, that sits under both of them.
`witchy-types` produces a `CanonicalSourceView` and `witchy-wir` consumes it.
After the change, `witchy-wir` has no dependency on `witchy-types` at all.
Its tests that compile witchy source move to a crate that legitimately
depends on both.

## Motivation

In Rust, the unit of compilation is the crate. Splitting a file inside a crate
changes only incremental codegen-unit granularity. What cargo can run in
parallel, and what an edit invalidates, is decided entirely by the crate graph.
The graph on `master` at `af1d1a941` is:

```
witchy-cap-model -> witchy-syntax -> witchy-caps -> witchy-types -> witchy-wir -> witchy-lower -> witchy
                                                                 \-> witchy-runtime -> witchy-interp -/
```

The median per-crate compile times come from the mbx build reports: syntax
50s, types 82s, wir 101s, lower 42s, runtime 33s, interp 28s. The serial chain
syntax -> types -> wir -> lower -> root adds up to roughly the median gate
build (259s), and that build is where the gate spends its time. The median
test stage is 90s.

Two costs follow from the `wir -> types` edge:

1. **Clean or upstream-invalidated builds serialize the two biggest crates.**
   wir cannot begin until types has emitted its metadata. That is 82s of
   critical path in which the 127k-line crate sits idle, on a host with cores
   to spare.
2. **Every typeck edit rebuilds wir, runtime and interp.** Nothing in wir reads
   typeck internals, but cargo cannot know that. A typeck edit today recompiles
   types, wir, runtime, interp, lower and root. Measured with
   `cargo test --workspace --no-run --timings` after a one-line
   `witchy-types/src/typeck.rs` edit (host load 60-80, so absolute numbers
   are inflated): 305s wall. `witchy-wir` recompiled as a library (82s), as
   its unit-test target (73s, the last unit to finish), and as eleven
   integration-test binaries (~30s). `witchy-runtime` recompiled twice (31s +
   24s). Of the 305s, only `witchy-types` (51s + 67s test), `witchy-lower`
   (48s + 46s test), `witchy-interp` (which depends on types directly) and the
   root package needed to rebuild.
   The mbx cache cannot help here, because these crates are genuinely
   downstream of the edit. The gate also bypasses mbx deliberately.

Typeck is the crate that compiler-perf work and language work touch most. The
agents editing it pay the wir+runtime rebuild on every inner-loop iteration.

## What wir actually uses

This census comes from `git grep witchy_types crates/witchy-wir/src`.
Non-test uses sit in seven files:

| File | Uses |
|---|---|
| `canonical_safewir/authority.rs` | `CanonicalCheckedSource`, `access::{AccessSignature, BorrowKind, AccessQualifier, OwnershipStateClass, LoanProjectionStep, CheckedAccessFacts}`, `canonical_aggregates::*`, `canonical_closure::ClosureCaptureTransfer`, `canonical_abi::CanonicalTargetPolicy`, `canonical_capabilities::representation`, `typeck::ty_to_ast` |
| `canonical_safewir/owner_authority.rs` | `canonical_aggregates::*`, `ClosureCaptureTransfer`, `ClosureBindingPathStep`, `ProperTailCallableId`, `ty_to_ast` |
| `canonical_safewir/closure_owner_contract.rs` | `CanonicalCheckedSource`, `canonical_capabilities`, `canonical_aggregates`, `ty_to_ast` |
| `canonical_safewir/vm_service.rs` | `canonical_worker::service_sites` |
| `canonical_safewir/history.rs` | `typeck::canonical_proper_tail_manifest_digest` |
| `canonical_safewir/optimize.rs` | `CanonicalCheckedSource` (pass-through) |
| `canonical_wasmir.rs` | `CanonicalCheckedSource`, `CanonicalTargetPolicy` |

Test modules also use `pipeline::link_checked_from_source_bundle` and
`CheckedModule` to compile witchy source into fixtures.

Of `CanonicalCheckedSource`, wir reads only these:

- `source_witness()`
- `launch_contract_payload()`
- `callable_abi_manifest()`
- `closed_callable_abi`, `closed_callable_signature`, `closed_callable_declaration`
- `callable_signature`
- `entry_declaration()` and `is_build_recipe()`
- `type_definitions()`
- `typed_module().module()` (the AST only)
- `access_facts()`
- `loan_facts()` (only `.is_ok()`)
- `closure_witness().entries()`
- `generic_specialization`
- `precompiled_declarations()`

The only checker-internal type anywhere in that list is the closure witness
signature, a `typeck::Ty`. wir reads it only through `ty_to_ast`, so the view
can carry the AST form instead.

## Design

### A contract crate: `witchy-canonical-source`

This is a new crate. Its dependencies are `witchy-syntax`, `witchy-cap-model`,
`foldhash`, `blake3` and `sha2`. It holds data and AST-level functions only,
with no inference and no `Ty`:

- `canonical_aggregates` (1.6k lines) moves here. It is AST-only apart from
  two helpers. The first is `anon_union_synthetic_variants`, the decoder for
  synthetic union names. Its encoder lives in `witchy-syntax/src/parser.rs`,
  and `witchy-syntax/src/format.rs` already carries a verbatim copy of the
  decoder. The codec therefore gets one home in `witchy-syntax`, and both
  copies are deleted. The second helper, `is_unit_type`, is an AST predicate
  and moves with it.
- `canonical_capabilities` (151 lines).
- The access *vocabulary*: `AccessSignature`, `AccessQualifier`, `BorrowKind`,
  `OwnershipStateClass`, `LoanProjectionStep`, and the `CheckedAccessFacts`
  data type. The builder that fills `CheckedAccessFacts` from a `TypeTable`
  stays in `witchy-types`. That is where `access.rs` splits: vocabulary down,
  inference up.
- `ClosureCaptureTransfer`, `ClosureBindingPathStep`, `ProperTailCallableId`,
  `canonical_proper_tail_manifest_digest`, `CheckedSourceWitness`,
  `CanonicalTargetPolicy`, `CanonicalCallableAbi(Manifest)`,
  `LogicalSpecializationIdentity`, and the worker service-site data type.
- `CanonicalSourceView`, described below.

These are moves, not copies. Every importer, including `witchy-types` itself,
is updated to the new path. There are no re-exports and no compatibility
layer.

### The view

```rust
/// What SafeWIR lowering and checking may read about one checked source.
/// Minted only by `witchy_types::pipeline` (see "Trust" below).
pub struct CanonicalSourceView<'s> {
    module: &'s ast::Module,              // the exact AST the facts are keyed on
    access: &'s CheckedAccessFacts,       // keyed by addresses inside `module`
    loans_checked: bool,
    type_definitions: &'s BTreeMap<String, TypeDef>,
    source_witness: &'s CheckedSourceWitness,
    launch_contract_payload: &'s [u8],
    callable_abi: &'s CanonicalCallableAbiManifest,
    closures: &'s [ClosureWitnessEntryView],   // AST signatures, not `Ty`
    specializations: &'s BTreeMap<String, LogicalSpecializationIdentity>,
    precompiled: &'s [PrecompiledExport],
    service_sites: &'s [ServiceSite],          // precomputed by the frontend
    build_entry: Option<&'s str>,
}
```

`CanonicalCheckedSource` keeps ownership of everything, as it does today, and
gains one method, `pub fn view(&self) -> CanonicalSourceView<'_>`. The two
digests and projections the view needs but the source does not yet store are
computed once in `attach_canonical_source`. Those are the AST closure
signatures, the service sites and the precompiled export tuples. The view
borrows everything, so producing it costs nothing, and the access facts can
never be paired with a different module than the one they were computed on.

Every public wir entry point that takes `&CanonicalCheckedSource` takes
`&CanonicalSourceView` instead:

- `check_initial_with_witness`
- `validate_final`
- `validate_optimized_final`
- `CheckedSafeWirRevision::validate_final`
- `CheckedPassOutcome::validate_final`
- `admit_canonical(_with_observer)`
- `SourceConstructor::from_checked` and `SourceConstructor::source`

The production callers are `witchy-lower/src/canonical_pipeline.rs` and
`canonical_lower.rs`. They call `source.view()`.

### Trust

Today, `CanonicalCheckedSource` cannot be constructed outside `witchy-types`,
and that is the property the authority chain relies on. A
`CheckedSourceWitness` can only be minted from a source the frontend produced.
Rust has no friend crates, so a type defined in the contract crate has a
constructor that any crate can call.

This RFC does not try to simulate friend visibility with a sealed-token trick.
Every crate in this workspace is inside the compiler's trusted computing base.
Rust visibility here protects against *mistakes*, not adversaries, and a
mistake is exactly what an architecture test catches. The design therefore
has three parts:

- `CanonicalSourceView` has private fields. It has exactly one constructor,
  `#[doc(hidden)] pub fn mint_from_checked_frontend(...)`.
- A workspace-wide `syn` test, modeled on
  `validated_safewir_has_one_checker_owned_construction_surface`, asserts that
  the only caller of that constructor is `CanonicalCheckedSource::view` in
  `witchy-types/src/pipeline.rs`. It also asserts that the view's fields stay
  private.
- The existing `a3_witness_and_digest_types_have_no_public_minting_surface`
  test moves its `CheckedSourceWitness` expectations to the new crate
  unchanged.

The authority argument is unchanged: initial SafeWIR corresponds to a
frontend-checked source. Only its enforcement moves from the type system to an
architecture test. The P2 review must also list every view fact that the
checker *trusts* rather than re-derives. The access facts and closure
transfers are the candidates. For each one, the review confirms that the
architecture test covers its only producer.

## Plan

Each phase lands separately through the queue.

1. **P1 — dev-dependency and mechanical moves.** Move the fixture-building
   test code onto a `witchy-types` dev-dependency. Create
   `witchy-canonical-source`. Move `canonical_capabilities`,
   `canonical_aggregates`, the closure/tail/ABI/witness data types, the access
   vocabulary and the digest function into it, and update every import. No
   behavior changes. The existing suites adjudicate this phase.
2. **P2 — the view.** Add `CanonicalSourceView` and
   `CanonicalCheckedSource::view`, precompute the three projections, switch
   the wir APIs and their two production callers, and add the architecture
   test. This phase touches the authority chain and needs an adversarial
   review.
3. **P3 — the cut.** Move `witchy-types` to `[dev-dependencies]` in
   `crates/witchy-wir/Cargo.toml`. Add an architecture test asserting that
   `cargo metadata` shows no normal `witchy-wir -> witchy-types` edge. This
   gives the clean-build parallelism and stops `cargo build` from recompiling
   wir and runtime.
4. **P4 — tests that compile source leave wir.** Cargo applies a
   dev-dependency to every test target of a crate. As long as any wir test
   compiles witchy source through `witchy-types`, a typeck edit still rebuilds
   wir's 73s unit-test target and its integration tests. The unit-test modules
   in `authority.rs` (~19 uses), `owner_authority.rs`,
   `closure_owner_contract.rs` and the two integration tests that build
   fixtures from source move to a test crate that depends on types, lower and
   wir, or to `witchy-lower`'s tests when they exercise only public API.
   Tests that need wir-private items get a `#[doc(hidden)]` test surface
   gated by the existing `raw-encoder-test-api` style feature. Afterwards,
   `witchy-types` disappears from wir's dev-dependencies too.

## Acceptance

- `cargo tree -p witchy-wir -e normal` does not list `witchy-types`.
- After a one-line edit to `witchy-types/src/typeck.rs`,
  `cargo build --workspace --timings` does not recompile `witchy-wir` or
  `witchy-runtime` (P3). After P4, neither does
  `cargo test --workspace --no-run`.
- In a clean `--timings` build, `witchy-wir` starts after `witchy-syntax`,
  not after `witchy-types`.
- Existing SafeWIR authority tests pass unchanged, apart from import paths,
  and the new construction-surface test pins the view's single minting site.

## Implementation and evidence

All four phases landed on 2026-09-24:

| Phase | Commits on `master` |
|---|---|
| P1 | `885d71a62`, `bbb6c6972`, `29fdcab78`, `5c9d28ca0`, `78d225650` |
| P2 | `7e328dcfc` |
| P3 | `bce978dd9` |
| P4 | `e71dd59b1` |

The implementation differs from the design above in these places:

- **Where P1's steps landed.** P1 put the contract types in the new crate.
  The dev-dependency move it listed happened in P3.
- **Constructor names.** The witness is minted by
  `CheckedSourceWitness::mint_for_checked_frontend` and the view by
  `CanonicalSourceView::from_checked_frontend`. The build-recipe path rebinds
  the launch digest with `rebind_launch_contract_for_build_recipe`. All three
  are `#[doc(hidden)] pub`.
  `crates/witchy-canonical-source/tests/checked_frontend_minting.rs` pins each
  one to its single call site in `witchy-types/src/pipeline{,/build_recipe}.rs`.
  It scans every tracked `.rs` file, including identifiers inside macro token
  streams, and asserts that both types keep private fields.
- **Access facts stay in `witchy-types`.** The access *vocabulary*
  (`AccessSignature` and its parts) moved down. `CheckedAccessFacts` itself
  stays with its builder. wir queries it through `CheckedAccessQuery`, whose
  two methods are `declaration` and `call_at`. That is a trait the checker
  implements, which the alternatives below reject as a way to *mint* source
  authority. Here it grants none: the only way to obtain one is
  `CanonicalSourceView::access_facts()`, from a view the frontend minted.
- **Closure witnesses carry an AST signature.** `ClosureWitness::signature`
  is an `Option<Type>` converted from `Ty` when the witness is built, so the
  view borrows the whole `ClosureWitnessManifest` rather than a projection.
  Service sites are computed from the view by
  `witchy_canonical_source::service::service_sites`, not stored.
- **The P3 metadata assertion is `check-stage-dependencies.py`.** It
  compares every workspace crate's dependencies against
  `spec/stage-dependencies.json`. Since P4, a `witchy-wir -> witchy-types`
  edge of any kind, including a dev-dependency, fails it.
- **The P4 test surface.** Of the tests that compiled source, 19 keep their
  bodies in wir as `pub fn <name>(checker: &dyn SourceChecker)`. They live in
  `canonical_safewir/{authority,owner_authority,closure_owner_contract}/checked_source_cases.rs`
  behind the `checked-source-test-api` feature, and only dev-dependency
  entries enable that feature. `crates/witchy-lower/tests/safewir_checked_source.rs`
  runs them against the real checker under their original names, together
  with three tests that moved whole. The wir route-gate test rejects the
  feature anywhere outside a dev-dependency table. The move preserved the
  test count (49 `#[test]` markers before and after), and no assertion was
  weakened. A fresh-context adversarial review accepted both P2 and P4.

Measured on `master` at `e71dd59b1` in a warmed worktree, through the `mbx`
cargo shim, at load 35-42 on 10 cores:

- **The edge is gone.** In plain `cargo tree -p witchy-wir`, neither
  `-e normal` nor `-e dev` lists `witchy-types`, and neither does the
  `-e normal` tree of `witchy-runtime --features native`. (The repo's cargo
  shim unifies `-p` across the workspace, so it prints every member's tree and
  would show `witchy-types` misleadingly.)
- **A checker edit no longer rebuilds wir or runtime.** After appending one
  line to `witchy-types/src/typeck.rs`:
  - `cargo build --workspace` recompiled `witchy-types`, `witchy-lower`,
    `witchy-interp` and `witchy`, in 118.6s.
  - `cargo test --workspace --no-run --features witchy-wir/native` recompiled
    the same four crates' targets, in 330.5s.

  Neither run recompiled `witchy-wir` or `witchy-runtime`. Before the cut, the
  same edit also rebuilt those two, and wir alone cost 82s (lib) plus 73s
  (unit tests). That run was at load 60-80, so its wall time does not compare
  directly with these.
- **wir now builds alongside the checker.** After a one-line edit to
  `witchy-syntax`, which invalidates every stage above it,
  `cargo build --workspace --timings` shows:
  - `witchy-canonical-source` starting at 3.4s, on syntax's metadata;
  - `witchy-wir` at 10.8–75.0s and `witchy-types` at 36.2–109.6s, so the two
    largest crates now compile concurrently;
  - `witchy-runtime` finishing at 88.2s, before `witchy-types` does.

  wir starts before types does, not after it. The whole build took 136s.

  The clean-build criterion was measured with this syntax-edit build, which
  invalidates every crate above syntax. A cold target is 30-60 GB on this
  machine.

## Alternatives

- **Split giant files.** This has no effect on either cost. The crate is the
  unit of compilation.
- **A trait that wir consumes and types implements.** This is the same
  forgeability as a public constructor, with less clarity: any crate can
  implement it. Pinning the construction site with a test is simpler and
  easier to audit.
- **Move `CanonicalCheckedSource` itself down.** It owns a `TypedModule` full
  of `Ty`. Moving it would drag the checker's type representation into the
  contract crate, and the contract crate would then be most of `witchy-types`.
- **Rely on mbx.** mbx already caches every crate that is not downstream of
  the edit. The cost described here is genuine downstream invalidation, which
  no cache removes.

## Drawbacks

- The workspace gains one crate.
- The authority boundary for source construction moves from the type system to
  an architecture test.
- P1 touches import lines across types, wir, lower and root. That makes merge
  conflicts with in-flight branches likely, so P1 should land quickly.
