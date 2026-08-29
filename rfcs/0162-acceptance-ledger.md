# RFC-0162 implementation and acceptance ledger

This is the live implementation ledger for RFC-0162. The RFC's acceptance
table is the contract; this file records current code evidence, dependencies,
owners, and terminal merge evidence. A row is complete only when its named
tests and artifacts prove the whole row on current `master` and every supplying
queue item has a terminal `merged` event.

## Current-truth checkpoint

- Checkpoint: 2026-08-27.
- RFC proposal tip: `f8ffd67b207e32715adb48c759d7801db24ff334`;
  terminally merged by the queue in master batch `29bd6c660`.
- Master observed before implementation: `a787a671e`; refreshed during the
  first implementation checkpoint to `416aa0e86` after the RFC-0161 integration
  batch landed. The integration branch was most recently rebased onto shared
  master `965176971`.
- Integration branch: `impl/rfc0162-integration` in
  `.claude/worktrees/rfc0162-validated-safewir`.
- Current encoder authority still contradicts the target contract:
  `wir_encode::encode` and `try_encode_with_gc*` accept `&WirModule` directly.
- Landed RFC-0153 range environments still use mutable names, but reviewed
  branch `impl/rfc0162-range-binding` replaces name authority with exact
  `DefinitionId`, `GenerationId`, object, index, and length-source bindings.
  That branch is not yet terminally merged or consumed by the production gate.
- The original integration base resolved `wasm-opt` from `PATH`. Digest-pinned
  version-131 resolution and production-pipeline evidence terminally merged in
  master batch `32a0cd108`; four-target release CI remains pending.
- The RFC's 478-site raw-memory baseline was stale. The corrected structural
  parser on `impl/rfc0162-structural-census` finds 584 candidate-WIR constructor
  sites in 25 production files, excluding the encoder's own
  `wasm_encoder::Instruction` construction. An initial 255 result was rejected
  because `syn` treats `vec![...]` bodies as opaque and because externally
  included encoder-test modules were counted as production. The 584
  per-file/per-operation ceiling now scans every workspace crate `src/` tree,
  treats every non-`None` raw body as privileged, fails closed on opaque
  constructor macros, and is locally green after recovery at commit `40be21d4a`.
  Its exact two-test census, ignored-test policy, and 44-test `witchy-wir` shard
  are green; queue item
  `mq-45f0101631194f18708a32305f03a91a92bdf6e4` is red and requires diagnosis
  and resubmission.
  The older
  `state/agents/wir-helper-census.md` is an API-visibility census and is not
  evidence for this row.
- SEC-078 landed on master as `da098299a` from submitted commit `dbee68461` on
  `security/fix-sec078-call-result-uaf`, queue item
  `mq-12fe973edfc236ae281c636efab5c4b41df0c32f`. The isolated full gate was
  green with zero flakes. The final focused evidence is 42 escape-analysis
  tests, one RC-floor regression, and three SEC-078 differential fixtures. A
  detached pre-fix worktree at `a787a671e` fails the same source reproducer
  with `arbitrary call result admitted as fresh storage`. Final Opus verdict is
  ACCEPT after the alias-laundering and result-storage blockers were fixed.
- The schema-1 fact model and independent judgment are committed after rebase
  as `3189cb910` on `impl/rfc0162-safewir-core`. Nine focused schema tests and
  the 51-test `witchy-wir` shard are
  green. Fable accepted the revised schema after its concrete required wording
  fixes were applied; the Wasm shard is green.
  The first two individual gate attempts had green workspace tests and failed
  only on an unrelated em dash in `book/src/capabilities-sandbox.md`; master
  fixed that gate violation in `12511ac71`. The unchanged schema commit was
  rebased as `0d369c5cc` and resubmitted under queue item
  `mq-93d52047f7342ae862c5ef99d3508078d24ab77f`, then terminally merged as master
  batch `a110a53b0`. Four-target CI release evidence remains pending.
- Fable and Opus both rejected the first `impl/rfc0162-validator` prototype.
  The valid blockers were candidate-attested callee shape, borrowed-to-Fresh
  laundering, asymmetric Shared-result checks, and a host-import namespace
  bypass. The rewritten closed-world constant report slice derives parameter
  shape and provenance, requires body-minted Fresh results, separates host
  imports, validates host ranges, rejects recursive/CFG/symbolic/non-root-alias
  forms as explicitly unsupported, and reports deterministic budget metrics.
  After two Fable rejections and Opus adjudication, alias-equivalence and
  alias-aware effect fixes close the final straight-line blockers. The rebased
  commit is `0a4feb592`; 75 `witchy-wir` library tests and bug-tier clippy are
  green, and Fable's final verdict is ACCEPT for this declared subset. Queue
  item `mq-e7d7db4cab5c25798d533227a8cb475d1181af3f` terminally merged in master
  batch `154ea94be`. Mutation completion, authenticated source/import/layout
  inputs, and the production gate remain pending. This bounded report slice
  alone supplies no production-gate row.
- Commit `bcbdde53a` on `impl/rfc0162-binaryen-pin` removes ambient `PATH`
  resolution in favor of an
  absolute executable path plus SHA-256, owner-only digest-addressed staging,
  version 131, and executable/version/recipe/input cache identity. Tests,
  adversarial review, and an authenticated official macOS arm64 v131 smoke are
  green: focused native runtime 8 passed, 1 intentionally ignored; final Opus
  verdict ACCEPT after Fable/Opus blockers were resolved. Follow-up commit
  `3ed57023c` installs the pinned bundle in release workflows and adds an exact
  pre/post production differential corpus: 13 independently adjudicated cases
  covering SEC-036/037, SEC-038 positive and negative bounds, SEC-039, SEC-078,
  and eight deterministic generated adversarial modules. Runtime Binaryen
  tests are 10 passed with one expected ignore; release/installer tests are
  three passed with one expected ignore; the official bundle smoke and package
  test each pass. Commit `43720e967` separates compiler-only and
  reviewed-production performance lanes and carries a local installed-release
  smoke. The complete focused evidence includes 27 runtime tests, seven misc
  selections, 928 examples, the Wasm shard, authenticated official v131
  package execution, and three scalar repetitions in each lane. Final Fable
  review ACCEPTS. Queue item
  `mq-09b6bfac40beab6a9977a3e209caf70844c963ff` is ready at follow-up commit
  `8871d3f47`, which repairs the packed-aggregate performance gate by making
  its compiler-only and authenticated reviewed-Binaryen lanes explicit. The
  exact compiler-only test, exact required-pinned-Binaryen test, and focused
  bug-tier clippy are green; the production lane retains the zero-GC-heap
  assertion. Queue item `mq-09b6bfac40beab6a9977a3e209caf70844c963ff`
  terminally merged in master batch `32a0cd108`; four-target CI release evidence
  remains pending.
- Original commit `ac4932bda`, now rebased as `217a5bd65` on
  `impl/rfc0162-sec-corpus`, stacks after the pinned
  Binaryen track, makes the applicable source and production-pipeline corpus
  permanent. SEC-036 is restored as the distinct alias-initialized
  free-at-overwrite class across variable, field, index, list, and historical
  TOML/`rfind` paths; SEC-037 remains the retained `contents.lines()` minigrep
  trigger; SEC-038 covers positive and negative byte bounds; SEC-039 covers
  non-object literal overwrite; and landed SEC-078 remains the call-result
  alias case. Four RC source tests, one exact SEC-038 test, the 15-case manifest,
  and the exact pinned-Binaryen pre/post production corpus are green. Fable's
  initial rejection and Opus's hardening findings were resolved; final Opus
  verdict is ACCEPT. Queue item
  `mq-0c523f31f4476622cbbcb3a1ba96738cb1317f82` depends on the Binaryen pin.
  Its first attempt conflicted after that dependency landed; the patch is now
  identically rebased and the exact production differential selection is 2/2.
  Two broad examples attempts were invalidated by severe concurrent host load,
  including timeouts at their configured 480- and 1200-second limits. A quiet
  rerun and resubmission remain pending. Forged-WIR rejections and
  encoder-template evidence remain separate and row 11 is not complete.
- Commit `cfab0454b` on `impl/rfc0162-wir-surface` introduces untrusted
  executable-side SafeWIR object, projection, provenance, extent, range, and
  access claims plus semantic load/store/copy/fill variants. Every optimizer,
  call-summary, inliner, tail-call, renaming, effect, and lowerer visitor now
  traverses all claim expressions. Ordinary encoding rejects these variants;
  exact WAT tests prove erasure shape parity without granting authority. Fable
  found production-reachable wildcard visitor gaps; the repaired diff has 81
  `witchy-wir` library tests green, `witchy-lower` compiles, bug-tier clippy is
  green, and final Opus verdict is ACCEPT after Fable's re-review timed out.
  This is a typed surface only: no production adapter, mandatory gate, raw-site
  migration, or acceptance row is complete.
- Commit `f4171b513` on `impl/rfc0162-wir-adapter`, stacked on the semantic
  surface, adds a final-WIR-to-declarative-candidate adapter for a deliberately
  small report-only subset. The adapter accepts only straight-line void
  functions with exact parameter-rooted scalar memory operations. It rejects
  raw memory and raw bodies, mutable locals, control flow, symbolic places,
  object results, value returns, unknown parameter shape, undeclared import
  authority, cross-representation access/copy, and typed fill. Fable rejected
  a memory-parameter escape omitted from the candidate graph; Opus then
  rejected vacuous value-result contracts. Both classes now fail closed, and
  final Opus review is ACCEPT. The final 90-test `witchy-wir` library shard and
  bug-tier clippy are green. This remains diagnostic evidence only: it exposes
  no `ValidatedWirModule`, encoder authority, source authentication, CFG
  fixpoint, or production gate, so no acceptance row is complete.
- Commit `520db2aec` on `impl/rfc0162-cfg-validator`, stacked on the report
  adapter, resolves final-WIR local indices to validator-owned definitions and
  validates structured void branches with conservative joins. Duplicate
  structural definition and generation sites reject before dataflow; alias
  effects are unioned across paths; conditional consumption is absorbing.
  The 93-test `witchy-wir` library shard and bug-tier clippy are green, and
  final Opus verdict is ACCEPT after Fable's duplicate-identity and
  branch-local-alias findings were fixed. Successor commit `d6825e3da` replaces
  candidate-attested loop joins with validator-owned conservative loop
  fixpoints, charges every re-evaluation to the operation budget, bounds
  structured depth before recursive descent, and uses iterative preflight
  traversals. Stable, consuming, reuse, temporary-lifetime, escape-effect,
  budget, and maximum-depth fixtures pass; the full `witchy-wir` library shard
  is 99/99 and bug-tier clippy is green. Fable's final verdict is ACCEPT. Queue
  item `mq-8168682a32da172075b8c0ada018f85dd2e9c842` terminally merged in master
  batch `154ea94be`. Value-return exits, symbolic range facts, and recursive
  components remain pending.
- Commit `d88854a19` on `impl/rfc0162-cfg-adapter`, stacked on the loop
  validator, maps final WIR `Block`, `Loop`, `Br`, and void `Return` into
  validator-owned lexical `ControlId`s. Block exits, loop back edges, outer
  exits, and void returns have separate conservative accumulators; provisional
  loop exits are discarded until the header fixpoint is stable; simultaneous
  pending exits plus fallthrough count against the fact budget. Non-enclosing,
  duplicate, and shadowed targets reject. The 65-test SafeWIR selection and
  107-test WIR library shard pass, bug-tier clippy is green, and final Fable
  review is ACCEPT after direct flow-merge, return-contract, and aggregate-fact
  mutation fixtures were added. Queue item
  `mq-94ebb9ae02192c6c42a01f39feb9a6beedc2de98` terminally merged in master
  batch `154ea94be`. Data-dependent mutable-local conditions and value-return
  exits remain in their owning def-use/callable tracks.
- Final squashed commit `9ac93aaf4` on `impl/rfc0162-range-binding`, rebased on
  current master, replaces mutable-name-only range authorization in the optimized
  indexed-kernel path with validator-owned definition, object, generation,
  index, and exact-length-source bindings. Reassignment, wrong-object,
  wrong-generation, wrong-length-source, join, and loop invalidations reject;
  its first queue attempt red twice on
  `redundant_comparison_simplification`: `RangeFact::from_bounds(11, MAX)`
  incorrectly collapsed to a `Positive` representation whose lower bound was
  only 1. Later cross-track review found stale post-evaluation `SetLocal` facts,
  condition effects entering branches with live object authority, missed
  `Control`-wrapped writes, kernel visitor gaps, and wrap-unsound unbounded
  Add/Mul facts. The final one-commit tree rejects every case, uses the single
  canonical refinement implementation, emits i32 comparison results, and has
  167 WIR tests green. Fable rejected the intermediate repair; final Opus and
  Fable verdicts are both ACCEPT. Required examples/Wasm reruns, queue
  resubmission, whole-pipeline pass combinations, terminal merge, and final
  production authority remain pending.
- Commit `fd8a01d62` on `impl/rfc0162-call-contracts`, stacked on the CFG
  adapter, validates direct, indirect, recursive, closure, projection, imported,
  stale-summary, and malformed callable contracts against the final candidate
  call graph. Projection bounds, closure arity, and callable-summary resource
  budgets reject fail closed. The complete 16-case callable matrix, 124-test WIR
  library shard, and bug-tier clippy are green. Fable and Opus rejected earlier
  revisions; every concrete finding was repaired and final Opus review is
  ACCEPT. Queue item `mq-a719ce6e805cea21295536110d7b41834b87c608`
  terminally merged in master batch `4c25e171e`. Binding authenticated catalog
  entries to their final production kernel/host definitions and final encoder
  authority remain integration requirements, so this is landed partial
  evidence rather than a completed acceptance row.
- Commit `d799089dc` on `impl/rfc0162-checked-entry`, stacked on the CFG adapter,
  closes a raw-module substitution seam in compiler-generated Glamour
  execution. The application rewrite is now an opaque value whose sole
  production constructor clones and semantically checks its exact
  `CheckedModule`; the generated adapter is metadata-restricted, rechecked with
  that authority, and compiled only through a second opaque checked value. The
  deleted three-argument encoder can no longer pair arbitrary application WIR
  with a convenient source catalog. Two boundary tests, two real static-site
  producer tests, both bug-tier clippy shards, and final Opus review are green.
  Queue item `mq-8e1842aadbae666467868359f787351ae8253599`
  terminally merged in master batch `4c25e171e`. This is a narrow
  source-authority slice: ordinary codegen still lacks one final
  `ValidatedWirModule` gate, and exact
  source-layout contradiction checking is supplied by a later stacked
  candidate rather than this commit.
- Commit `cee9ed3b3` on `impl/rfc0162-generic-layout-binding`, stacked on the
  checked-entry branch, gives every logical specialization identity its exact
  concrete `Type` and canonical `LayoutId`, rejects missing or conflicting
  bindings, and retains per-instance layout roots after function lowering.
  The evidence is 359 type tests, 171 lowerer tests, 16 specialization tests,
  bug-tier clippy, and final Opus ACCEPT after Fable rejected an earlier
  incomplete binding. Queue item
  `mq-9d744b5a94e641be7f7891babbd449b1152c07f3` terminally merged in master
  batch `32a0cd108`; final encoder consumption remains pending.
- Commits through rebased `922330df3` on `impl/rfc0162-source-layout` stack an opaque source
  layout contract on the checked and generic authorities. It derives exact
  logical shapes from the linked, resolved source graph, rejects compiler
  evaluation type-graph changes, compares every selected `(Type, LayoutId)`
  recursively against a freshly decoded canonical bundle, and retains generic
  roots through final layout publication. Borrow-qualified evidence is
  normalized only at the top-level pointee boundary; a real checked generic
  compilation proves that path is non-vacuous. Focused type, lowerer, WIR,
  specialization, source-authority, compiler-evaluation, combined 843-test,
  bug-tier clippy, and Wasm shards are green. The final commit also makes the
  RFC-0110 visitor traverse every SafeWIR claim; that identical patch is now
  upstream through the mutation-evidence merge. Its exact seven-test shard is
  green and final Fable review ACCEPTS. A concurrent broad examples run passed
  925 of 928 tests but left three load-sensitive timeouts, so a quiet rerun plus
  the web-focused shard and queue submission remain pending. This is not
  terminal evidence and does not supply `ValidatedWirModule` authority.
- Original commit `9058e272f` on `impl/rfc0162-kernel-ops`, stacked on the semantic
  surface, adds opaque bump/RC allocation, retain, release, destructive reuse,
  and reclaim operations. They cannot carry a `WirFunc`, raw body, or ordinary
  implementation sequence; all optimizer and lowerer visitors traverse their
  operands and claims. Ordinary encoding rejected them without authority when
  this was written; that is no longer true of the PRODUCTION entrypoint. The
  RFC-0162 migration bridge (`ErasureMode::TransitionalBridge`) erases SafeWIR
  on `try_encode_with_gc_source_map` so a partially migrated module still
  encodes, and the rejection now survives only on the test-only
  `try_encode_with_gc`. Row 15 is not satisfied until that bridge is sealed.
  Five
  focused kernel tests and the earlier 86-test WIR library shard pass. Fable's
  missed-visitor rejection and Opus's two visitor-test findings were fixed;
  final Opus verdict is ACCEPT. After resolving a queue conflict on current
  master, final commit `19427b3c5`
  passed 130 WIR tests, 332 lowerer tests, seven RFC-0110 tests, serialized
  timeout reproductions, the Wasm shard, bug-tier clippy, and two Fable reviews.
  Queue item `mq-e2a54035340a435eae5926bbfaf1b3d913ec3902`
  terminally merged as master `19427b3c5`. Fixed templates, migration, and
  production authority remain pending.
- Commit `bfeb20c13` on `impl/rfc0162-kernel-templates`, stacked on the opaque
  kernel operations, adds the fixed encoder-owned templates for allocation,
  retain, release, destructive reuse, and reclaim. Ordinary WIR still cannot
  carry these privileged bodies. Five exact structural/template tests, 10
  native boundary/poison/reuse tests, the 91-test WIR library and Nextest
  shards, bug-tier clippy, and 311 downstream lowerer tests are green. Fable's
  nested-reuse scratch, checked-heap, and coverage findings were fixed; final
  Opus verdict is ACCEPT. Queue item
  `mq-509c806a9c9b0c3abdf6ceb3e1e1cf6ee57211c7` depends on the opaque-operation
  branch. Its first attempt conflicted after that dependency landed and must be
  rebased and resubmitted. Production `ValidatedWirModule` authority,
  ordinary-operation migration, and terminal queue evidence remain pending.
- Commit `3c080c4c9` on `impl/rfc0162-region-kernel` adds the one opaque
  `RegionMark` expression and the three fixed iteration, lexical, and copy-out
  rewind shapes. It preserves the existing operand, checked-heap hook,
  overlap-safe copy, frontier, free-list, poisoning, and reuse order; adds
  missing-global, out-of-bounds, and unsafe iteration/free-list negatives; and
  leaves telemetry outside the opaque operation. Fable's final verdict is
  ACCEPT. `SWIR-KERN-002` now specifies the exact LIFO watermark and rewind
  judgment. The candidate is statically clean but deliberately has no credited
  build result while host load is saturated; focused WIR/lowerer, template,
  Wasm, examples, and terminal queue evidence remain pending.
- Commits `8cad4c2e2` and `bd640656f` on
  `impl/rfc0162-validated-encoder-authority` add a private, non-Clone
  `ValidatedWirModule`, fail-closed final validation, mechanical fixed-template
  encoding, mandatory wasmparser validation, and `SWIR-GC-001` binding of the
  exact recursive struct/array declarations and every GC-sensitive use. Raw
  operation, raw body, memory grow/size, declaration substitution, index-band
  confusion, field mutability, cast, constructor-arity, and budget negatives
  reject. The 113-test WIR shard, three compile-fail API proofs, feature checks,
  bug-tier clippy, and final Fable review are green. This branch is deliberately
  unsubmitted: source authority, landed templates, region contracts, and the
  lowerer's transitional raw source-map bridge still require integration.
- Commit `11b90fef1` on `impl/rfc0162-mutation-evidence` terminally merged as
  master under queue item `mq-4fef5e9b08e7b7d2ab7a6329ab90e90c54c34fff`.
  Its deterministic schema records 20 caught, zero surviving, zero timed-out,
  and zero unviable mutations. Fifteen report-mode kills are credited; five
  production sentinels (`SWIR-MOD-001`, `SWIR-RNG-002`, `SWIR-HOST-001`,
  `SWIR-KERN-001`, and `SWIR-ENC-001`) remain deliberately uncredited until the
  final production gate exists. Final Fable verdict is ACCEPT.

## Dependency graph and tracks

```text
T0 current census + normative rule map + evidence harness
 |                    |
 v                    v
T1 SafeWIR facts   T2 callable contracts + DefinitionId range binding
 |                    |
 +----------+---------+
            v
T3 final validator in diagnostic/report mode
 |                    |
 +----------+---------+
 |                    |
 v                    v
T4 ordinary ops    T5 sealed kernel + mechanical encoder
 +----------+---------+
            v
T6 mandatory ValidatedWirModule production gate
            |
            v
T7 pinned Binaryen + SEC/fuzz/mutation/zero-cost evidence
            |
            v
T8 adversarial review + integration + terminal queue audit
```

Tracks may land independently only when their public contracts and dependency
edges are explicit. No track may introduce a second range lattice, a public raw
encoder escape hatch, or runtime safety metadata.

| Track | State | Branch / owner | Required output | Verification / terminal evidence |
|---|---|---|---|---|
| T0 census, rule map, ledger | red census candidate | `impl/rfc0162-integration`, `impl/rfc0162-structural-census` | reproducible production raw-memory/raw-body/callable/layout census; normative rule IDs; RichWasm correspondence | all-crates 584-site no-growth test, ignored-test policy, and 44-test shard green at `40be21d4a`; queue red requires diagnosis and resubmission |
| T1 SafeWIR fact model | landed core/generic plus stacked source candidate | master; `impl/rfc0162-source-layout` | object, place, range, layout, provenance, ownership, definition, generation, and exact checked-source layout identities | core and generic binding terminally merged through batch `32a0cd108`; exact source-layout authority is identically rebased through `922330df3` with 843 combined tests plus Wasm and final Opus/Fable ACCEPT; quiet examples rerun and queue terminal pending |
| T2 callable and range binding | landed SEC/callable slices; final range candidate awaiting shards | SEC-078 master `da098299a`; callable master batch `4c25e171e`; `impl/rfc0162-range-binding` | checked callable contracts; `DefinitionId`-keyed RFC-0153 facts tied to generation and length source | SEC-078 and callable matrix landed; final squashed range identity, evaluation-order, wrap, kernel traversal, and invalidation repairs are green at `9ac93aaf4` with 167 WIR tests and final Opus/Fable ACCEPT; examples/Wasm, terminal merge, and production catalog binding remain incomplete |
| T3 final validator | landed report slices | master batch `154ea94be` | deterministic bounded dataflow/fixpoint validator with rule diagnostics and accepted/rejected fixtures | declarative subset at `0a4feb592`, final-WIR report adapter at `f4171b513`, joins/loops through `d6825e3da`, and real WIR exits at `d88854a19` terminally merged; 107/107 WIR library tests and bug-tier clippy green; final Fable/Opus verdicts ACCEPT; mutable-local conditions, symbolic facts, recursive components, and production authority remain incomplete |
| T4 ordinary operation migration | candidate interface | `impl/rfc0162-wir-surface` | all non-kernel helpers and lowering use semantic SafeWIR operations | semantic variants and complete visitor symmetry at `cfab0454b`; 81 library tests and lowerer compile green; zero production constructors migrated |
| T5 kernel and encoder | landed opaque ops; template and region candidates | master `19427b3c5`; `impl/rfc0162-kernel-templates`; `impl/rfc0162-region-kernel` | minimal opaque kernel templates; encoder accepts validated authority only; exact encoding tests | six opaque operations terminally merged at `19427b3c5`; rebased fixed-template candidate `c176a5c42` plus uncommitted profile/fingerprint hardening and region candidate `3c080c4c9` have static Fable ACCEPT verdicts; build validation, production authority, migration, and terminal queue evidence remain pending |
| T6 mandatory production gate | reviewed authority candidate awaiting integration | master batch `4c25e171e`; `impl/rfc0162-source-layout`; `impl/rfc0162-validated-encoder-authority` | private `ValidatedWirModule`; no unchecked production encode/raw-body path | checked source entry landed; exact source-layout candidate rebased through `922330df3`; private authority plus exact GC declarations is green through `bd640656f` with 113 WIR tests, API proofs, and Fable ACCEPT; source/templates/region integration, raw bridge removal, and queue terminal remain pending |
| T7 external TCB and evidence | Binaryen and report mutation evidence landed; SEC rebased | master batches `32a0cd108` and `11b90fef1`; `impl/rfc0162-sec-corpus` | pinned Binaryen; SEC, mutation, fuzz, poison/reuse, zero-cost, and compiler-cost artifacts | Binaryen terminally merged; 20 modeled mutations are caught with 15 report kills credited and five production sentinels deferred; SEC corpus is rebased at `217a5bd65` with exact production comparison green, but load-contaminated examples reruns and resubmission remain pending; release CI, forged-WIR SEC negatives, fuzz, and zero-cost evidence pending |
| T8 review and closeout | pending | integration | Fable review, Opus adjudication if rejected, resolved findings, terminal merge audit | pending |

## Acceptance ledger

`pending` means the required evidence is absent or contradicted by current
code. `candidate` means reusable work exists but is not landed proof.

| # | Contract | State | Current evidence / contradiction | Evidence required to close |
|---:|---|---|---|---|
| 1 | Production codegen has one checked source entry | candidate | `d799089dc` binds the compiler-generated Glamour rewrite to its exact checked source; rebased source authority through `922330df3` rejects missing or contradictory selected/generic layouts against that graph. Public encoders still accept ordinary `WirModule`, quiet examples and queue terminal are pending | Test-only raw entrypoints; contradictory or missing source layouts reject through the final production boundary |
| 2 | SafeWIR carries object, layout, extent, provenance, ownership, and generation | candidate | Core fact schema and exact specialization identity terminally merged in `a110a53b0` and `32a0cd108`; reviewed source-layout binding remains unsubmitted and no final encoder consumes all identities | Accepted and forged fixtures for every fact family through final optimized WIR |
| 3 | RichWasm correspondence is explicit | candidate | `0162-safewir-judgment.md` and its named RichWasm/Witchy-extension mapping terminally merged in `a110a53b0`; complete production enforcement and final review remain pending | Every core rule enforced through the production validator or explicitly rejected as unsupported |
| 4 | Independent normative judgment | candidate | Schema-1 judgment and 20 stable IDs plus the bounded report validator, CFG joins, loop fixpoints, and real WIR exits terminally merged through `154ea94be`; complete mutation assurance and final production enforcement remain pending | Stable local, transfer, join, and fixpoint rule IDs; accepted/rejected/mutation fixtures per rule |
| 5 | Call results default to possible aliasing | candidate | SEC-078 landed as `da098299a`; the 16-case direct, indirect, recursive, closure, projection, imported, stale-summary, and malformed callable matrix terminally merged in batch `4c25e171e` with final Opus ACCEPT; final production catalog binding remains pending | Bind every authenticated host/kernel contract to the final production definition |
| 6 | Final optimized WIR checked once and fail closed | pending | Private authority and API-negative proofs exist through `bd640656f`, but are unintegrated; master encoding remains directly callable | Integrate the private authority after source/templates/region contracts and remove every production raw encoder entry |
| 7 | Optimizer combinations cannot retain stale facts | candidate | `9ac93aaf4` binds facts to exact identities and rejects reassignment, nested evaluation effects, condition effects, joins, loops, kernel operands, wrong object/generation/length source, and wrap-unsound arithmetic; whole-pipeline inline/specialize/recursion combinations and terminal merge remain incomplete | Inline, specialize, recursion, and complete pass-combination rejection fixtures through the production gate |
| 8 | Raw memory is sealed | pending | All-crates structural parser confirms 584 sites in 25 files; no-growth ceiling is locally green but migration remains zero of 584 | Current census reaches zero outside the named kernel-template module; structural test blocks regressions |
| 9 | Kernel is small and directly tested | candidate | The six landed allocation/RC shapes and the four-shape region candidate cannot carry ordinary WIR bodies; `c176a5c42` plus pending profile/fingerprint hardening supplies fixed templates, and `3c080c4c9` supplies region boundary/free-list/copy-out negatives with Fable ACCEPT; current-load build validation, production helper migration, mutation evidence, and terminal queue landing remain incomplete | Published operation census/contracts; poison, boundary, reuse, exact-template, and mutation tests |
| 10 | Encoder mechanically erases facts | candidate | `bfeb20c13` mechanically encodes opaque kernel operations through fixed templates, but the encoder still accepts arbitrary WIR and supports production raw bodies | Exact template tests, mandatory Wasm validation, and no production raw-body route |
| 11 | Applicable SEC corpus is permanent | candidate | Identically rebased `217a5bd65` restores distinct SEC-036 and pins SEC-037/038/039 source plus exact 15-case production-Binaryen evidence; SEC-078 is landed on master; quiet examples, terminal merge, forged-WIR, and encoder-template negatives remain pending | Source regression plus forged-WIR rejection for SEC-036/037/038/039/078; encoder test where applicable |
| 12 | Validator mutations are killed | candidate | `11b90fef1` terminally lands 20 caught and zero surviving/timed-out/unviable mutations; 15 report rules are credited, while five production sentinels remain uncredited because the production gate does not yet exist | Bind and kill the five production sentinels through the real final validator/encoder path |
| 13 | Binaryen trust is explicit and reproducible | candidate | PATH-free digest-pinned version-131 resolution, release bundle installation, exact recipe/cache identity and fallback tests, `PreserveRaw` exclusion, authenticated official/package/install smokes, reviewed-production performance lane, and a 13-case exact pre/post corpus terminally merged in `32a0cd108`; release-matrix CI evidence remains pending | Four-target release CI proves installation and exact required-pinned execution |
| 14 | Generated execution adds zero safety overhead | pending | No matched before/after artifact exists | Exact WAT and calls/loads/stores/branches/accesses/allocations/imports/globals/functions/fields/code-size/dynamic-count/benchmark evidence |
| 15 | Release claim is fail closed | pending | Unchecked production encoders still exist on master; `bd640656f` proves construction, post-validation mutation, and declaration substitution fail through the candidate API only | Deliberately invalid final WIR cannot encode through any production path |

## Evidence rules

- Backend agreement is necessary for stable semantics but an independent
  expected result adjudicates correctness.
- Tests are accepted only when their negative case reaches the production
  boundary named by the row; source grep alone is not validator evidence.
- Raw counts, queue state, performance, and compiler-cost measurements carry a
  source SHA, binary/tool identity, command, raw samples, and timestamp.
- Zero-cost comparisons use matched whole operations with unchanged optimization
  decisions. Equal helper-call count alone is insufficient.
- Validation wall time, peak memory, fact count, budget exhaustion, and semantic
  fallback rate are release metrics. Skipping validation is never a fallback.
- Fable reviews the final parity/ABI/security diff. If it rejects, Opus provides
  independent adjudication; every concrete finding is fixed or rejected with
  code evidence before submission.

## Closeout checklist

- [ ] Every acceptance row above is `merged` with current evidence.
- [ ] The refreshed raw-constructor census is zero outside the kernel template.
- [ ] Every validator rule has accepted, rejected, and mutation-killing tests.
- [ ] SEC and generated programs agree across interpreter, semantic Wasm,
      optimized pre-Binaryen Wasm, and the exact production pipeline.
- [ ] Zero-runtime-cost and validator-resource artifacts satisfy RFC-0162.
- [ ] Fable/Opus review findings are resolved.
- [ ] Every supplying queue item has a terminal `merged` event and its landed
      master SHA is recorded.
- [ ] RFC-0162 status changes only after this audit is complete.
