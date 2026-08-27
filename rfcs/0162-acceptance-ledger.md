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
  batch landed. Current integration rebase: `01130ba7a`; latest observed
  shared master is `5b438661d`, so another integration rebase is owed.
- Integration branch: `impl/rfc0162-integration` in
  `.claude/worktrees/rfc0162-validated-safewir`.
- Current encoder authority contradicts the target contract:
  `wir_encode::encode` and `try_encode_with_gc*` accept `&WirModule` directly.
- Current RFC-0153 range environments are keyed by mutable local names. They do
  not yet bind facts to `DefinitionId`, `GenerationId`, or an exact length
  source.
- The queued integration base originally resolved `wasm-opt` from `PATH`.
  Branch `impl/rfc0162-binaryen-pin` now carries a digest-pinned version-131
  bundle and production-pipeline evidence; terminal merge evidence is pending.
- The RFC's 478-site raw-memory baseline was stale. The corrected structural
  parser on `impl/rfc0162-structural-census` finds 584 candidate-WIR constructor
  sites in 25 production files, excluding the encoder's own
  `wasm_encoder::Instruction` construction. An initial 255 result was rejected
  because `syn` treats `vec![...]` bodies as opaque and because externally
  included encoder-test modules were counted as production. The 584
  per-file/per-operation ceiling now scans every workspace crate `src/` tree,
  treats every non-`None` raw body as privileged, fails closed on opaque
  constructor macros, and is locally green after rebase at commit `ac3a6017a`.
  Its exact two-test census and 44-test `witchy-wir` shard are green; queue item
  `mq-45f0101631194f18708a32305f03a91a92bdf6e4` is pending terminal evidence.
  The older
  `state/agents/wir-helper-census.md` is an API-visibility census and is not
  evidence for this row.
- SEC-078 is committed after rebase as `25d3fa4a6` on
  `security/fix-sec078-call-result-uaf`. The focused escape-analysis shard is
  green at 41 tests, the current source reproducer is green, and a detached
  pre-fix worktree at `a787a671e` fails the same reproducer with
  `arbitrary call result admitted as fresh storage`. Fable and Opus upheld two
  alias-laundering blockers; both were fixed, and final Opus verdict is ACCEPT.
  Post-rebase qualification is running under the serialized gate lock; queue
  and terminal merge evidence remain pending.
- The schema-1 fact model and independent judgment are committed after rebase
  as `3189cb910` on `impl/rfc0162-safewir-core`. Nine focused schema tests and
  the 51-test `witchy-wir` shard are
  green. Fable accepted the revised schema after its concrete required wording
  fixes were applied; the Wasm shard is green. Queue item
  `mq-93d52047f7342ae862c5ef99d3508078d24ab77f` is pending terminal evidence.
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
  item `mq-e7d7db4cab5c25798d533227a8cb475d1181af3f` depends on the core branch.
  Mutation completion, the production adapter, CFG/fixpoint, authenticated
  source/import/layout inputs, and terminal merge remain pending. This slice
  supplies no production-gate row.
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
  test each pass. Two final Fable reviews ACCEPT. Queue qualification, terminal
  merge, and four-target CI release evidence remain pending.
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
| T0 census, rule map, ledger | queued candidate | `impl/rfc0162-integration`, `impl/rfc0162-structural-census` | reproducible production raw-memory/raw-body/callable/layout census; normative rule IDs; RichWasm correspondence | all-crates 584-site no-growth test and 44-test shard green at `ac3a6017a`; queue terminal pending |
| T1 SafeWIR fact model | queued candidate | `impl/rfc0162-safewir-core` | object, place, range, layout, provenance, ownership, definition, and generation identities | 9 schema tests, 51-test crate shard, and Wasm shard green at `3189cb910`; revised Fable verdict ACCEPT; queue terminal pending |
| T2 callable and range binding | candidate dependency | SEC-078 commit `25d3fa4a6`; remainder unassigned | checked callable contracts; `DefinitionId`-keyed RFC-0153 facts tied to generation and length source | 41 focused tests, current source regression, detached pre-fix-red proof, and final Opus ACCEPT; qualification/queue/merge and remainder pending |
| T3 final validator | queued report candidate | `impl/rfc0162-validator` | deterministic bounded dataflow/fixpoint validator with rule diagnostics and accepted/rejected fixtures | closed-world constant subset at `0a4feb592`: 75/75 library tests and bug-tier clippy green, final Fable ACCEPT; queue terminal pending; unsupported CFG/symbolic/alias-set forms and production authority remain no row evidence |
| T4 ordinary operation migration | candidate interface | `impl/rfc0162-wir-surface` | all non-kernel helpers and lowering use semantic SafeWIR operations | semantic variants and complete visitor symmetry at `cfab0454b`; 81 library tests and lowerer compile green; zero production constructors migrated |
| T5 kernel and encoder | pending | unassigned | minimal opaque kernel templates; encoder accepts validated authority only; exact encoding tests | pending |
| T6 mandatory production gate | pending | integration | private `ValidatedWirModule`; no unchecked production encode/raw-body path | pending |
| T7 external TCB and evidence | candidate | `impl/rfc0162-binaryen-pin`; other evidence unassigned | pinned Binaryen; SEC, mutation, fuzz, poison/reuse, zero-cost, and compiler-cost artifacts | pin plus release integration and 13-case exact production corpus at `3ed57023c`; runtime/release/package smokes green and two Fable ACCEPT verdicts; qualification/terminal merge and non-Binaryen evidence pending |
| T8 review and closeout | pending | integration | Fable review, Opus adjudication if rejected, resolved findings, terminal merge audit | pending |

## Acceptance ledger

`pending` means the required evidence is absent or contradicted by current
code. `candidate` means reusable work exists but is not landed proof.

| # | Contract | State | Current evidence / contradiction | Evidence required to close |
|---:|---|---|---|---|
| 1 | Production codegen has one checked source entry | pending | Public encoders accept ordinary `WirModule` | Test-only raw entrypoints; contradictory or missing source layouts reject |
| 2 | SafeWIR carries object, layout, extent, provenance, ownership, and generation | pending | No complete fact carrier exists | Accepted and forged fixtures for every fact family |
| 3 | RichWasm correspondence is explicit | candidate | `0162-safewir-judgment.md` names the RichWasm sections and Witchy extensions; terminal review/merge pending | Every core rule maps to RichWasm or a reviewed named Witchy extension |
| 4 | Independent normative judgment | candidate | Schema-1 judgment and 20 stable IDs exist on queued `3189cb910`; the rewritten report validator partitions enforced and explicitly deferred rules, but has no complete mutation assurance and does not implement joins/fixpoints | Stable local, transfer, join, and fixpoint rule IDs; accepted/rejected/mutation fixtures per rule |
| 5 | Call results default to possible aliasing | candidate | SEC-078 candidate defaults arbitrary calls away from fresh for RC-floor analysis only | Land SEC-078 and cover direct, indirect, recursive, closure, projection, imported-call, and validator matrices |
| 6 | Final optimized WIR checked once and fail closed | pending | No `ValidatedWirModule`; encoding is directly callable | Private validated constructor and production encoder API negative tests |
| 7 | Optimizer combinations cannot retain stale facts | pending | RFC-0153 facts are local-name keyed | Reassignment, join, loop, inline, specialize, recursion, and pass-combination rejection fixtures |
| 8 | Raw memory is sealed | pending | All-crates structural parser confirms 584 sites in 25 files; no-growth ceiling is locally green but migration remains zero of 584 | Current census reaches zero outside the named kernel-template module; structural test blocks regressions |
| 9 | Kernel is small and directly tested | pending | Existing helpers are ordinary WIR functions | Published operation census/contracts; poison, boundary, reuse, exact-template, and mutation tests |
| 10 | Encoder mechanically erases facts | pending | Encoder accepts arbitrary WIR and supports raw bodies | Exact template tests, mandatory Wasm validation, and no production raw-body route |
| 11 | Applicable SEC corpus is permanent | candidate | SEC-038/039 tests exist; SEC-078 candidate exists; SEC-036 identity unresolved | Source regression plus forged-WIR rejection for SEC-036/037/038/039/078; encoder test where applicable |
| 12 | Validator mutations are killed | pending | Validator absent | Mutation matrix kills every rule deletion, inversion, or weakening |
| 13 | Binaryen trust is explicit and reproducible | candidate | `3ed57023c` supplies the PATH-free digest-pinned version-131 implementation, release bundle installation, exact recipe/cache identity and fallback tests, `PreserveRaw` exclusion, authenticated official smoke, and a 13-case exact pre/post production corpus; terminal merge and release-matrix CI evidence are pending | Pinned executable identity/recipe/cache key, explicit fallback, `PreserveRaw` exclusion, pre/post differential corpus |
| 14 | Generated execution adds zero safety overhead | pending | No matched before/after artifact exists | Exact WAT and calls/loads/stores/branches/accesses/allocations/imports/globals/functions/fields/code-size/dynamic-count/benchmark evidence |
| 15 | Release claim is fail closed | pending | Unchecked production encoders exist | Deliberately invalid final WIR cannot encode through any production path |

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
