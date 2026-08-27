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
  shared master is `1300c4508`, so another integration rebase is owed.
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
  constructor macros, and is locally green after recovery at commit `40be21d4a`.
  Its exact two-test census, ignored-test policy, and 44-test `witchy-wir` shard
  are green; queue item
  `mq-45f0101631194f18708a32305f03a91a92bdf6e4` is pending terminal evidence.
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
  fixes were applied; the Wasm shard is green. Queue item
  The first two individual gate attempts had green workspace tests and failed
  only on an unrelated em dash in `book/src/capabilities-sandbox.md`; master
  fixed that gate violation in `12511ac71`. The unchanged schema commit was
  rebased as `0d369c5cc` and resubmitted under queue item
  `mq-93d52047f7342ae862c5ef99d3508078d24ab77f`; terminal evidence remains
  pending.
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
  test each pass. Commit `43720e967` separates compiler-only and
  reviewed-production performance lanes and carries a local installed-release
  smoke. The complete focused evidence includes 27 runtime tests, seven misc
  selections, 928 examples, the Wasm shard, authenticated official v131
  package execution, and three scalar repetitions in each lane. Final Fable
  review ACCEPTS. Queue item
  `mq-09b6bfac40beab6a9977a3e209caf70844c963ff` is ready; terminal merge and
  four-target CI release evidence remain pending.
- Commit `ac4932bda` on `impl/rfc0162-sec-corpus`, stacked after the pinned
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
  Forged-WIR rejections and encoder-template evidence remain separate and row
  11 is not complete.
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
  item `mq-8168682a32da172075b8c0ada018f85dd2e9c842` depends on the report adapter.
  Value-return exits, symbolic range facts, recursive components, and terminal
  queue evidence remain pending.
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
  `mq-94ebb9ae02192c6c42a01f39feb9a6beedc2de98` depends on the loop validator.
  Data-dependent mutable-local conditions and value-return exits remain in
  their owning def-use/callable tracks.
- Commit `030af4e6b` on `impl/rfc0162-range-binding`, stacked on the semantic
  surface, replaces mutable-name-only range authorization in the optimized
  indexed-kernel path with validator-owned definition, object, generation,
  index, and exact-length-source bindings. Reassignment, wrong-object,
  wrong-generation, wrong-length-source, join, and loop invalidations reject;
  the final focused evidence is 101 WIR tests, 10 range tests, 17 strength
  reduction tests, 17 native range tests, and bug-tier clippy. Fable rejected
  the initial evidence; the repaired implementation received final Opus ACCEPT.
  Queue item `mq-2ea83326def32f306739e25483bf80822bef7859`
  depends on the semantic surface. Whole-pipeline pass combinations and final
  production authority remain pending.
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
  depends on the CFG adapter. This is a narrow source-authority slice: ordinary
  codegen still lacks one final `ValidatedWirModule` gate, and exact
  source-layout contradiction checking remains pending.
- Commit `9058e272f` on `impl/rfc0162-kernel-ops`, stacked on the semantic
  surface, adds opaque bump/RC allocation, retain, release, destructive reuse,
  and reclaim operations. They cannot carry a `WirFunc`, raw body, or ordinary
  implementation sequence; all optimizer and lowerer visitors traverse their
  operands and claims; ordinary encoding rejects them without authority. Five
  focused kernel tests and the earlier 86-test WIR library shard pass. Fable's
  missed-visitor rejection and Opus's two visitor-test findings were fixed;
  final Opus verdict is ACCEPT. Queue item
  `mq-e2a54035340a435eae5926bbfaf1b3d913ec3902` depends on the semantic surface.
  Fixed templates, validator judgments, migration, and production authority
  remain pending.

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
| T0 census, rule map, ledger | queued candidate | `impl/rfc0162-integration`, `impl/rfc0162-structural-census` | reproducible production raw-memory/raw-body/callable/layout census; normative rule IDs; RichWasm correspondence | all-crates 584-site no-growth test, ignored-test policy, and 44-test shard green at `40be21d4a`; queue terminal pending |
| T1 SafeWIR fact model | queued candidate | `impl/rfc0162-safewir-core` | object, place, range, layout, provenance, ownership, definition, and generation identities | 9 schema tests, 51-test crate shard, and Wasm shard green at `3189cb910`; revised Fable verdict ACCEPT; queue terminal pending |
| T2 callable and range binding | landed SEC slice; queued range candidate; active callable remainder | SEC-078 master `da098299a`; `impl/rfc0162-call-contracts`; `impl/rfc0162-range-binding` | checked callable contracts; `DefinitionId`-keyed RFC-0153 facts tied to generation and length source | SEC-078 source and differential evidence landed; range identity/invalidation evidence is green at `030af4e6b` with final Opus ACCEPT and queue terminal pending; call matrix remains active |
| T3 final validator | stacked report candidates | `impl/rfc0162-validator`, `impl/rfc0162-wir-adapter`, `impl/rfc0162-cfg-validator`, `impl/rfc0162-cfg-adapter` | deterministic bounded dataflow/fixpoint validator with rule diagnostics and accepted/rejected fixtures | declarative subset at `0a4feb592`, final-WIR report adapter at `f4171b513`, joins/loops through `d6825e3da`, and real WIR exits at `d88854a19`; 107/107 WIR library tests and bug-tier clippy green; final Fable/Opus verdicts ACCEPT; queue terminal pending; mutable-local conditions, symbolic facts, recursive components, and production authority remain incomplete |
| T4 ordinary operation migration | candidate interface | `impl/rfc0162-wir-surface` | all non-kernel helpers and lowering use semantic SafeWIR operations | semantic variants and complete visitor symmetry at `cfab0454b`; 81 library tests and lowerer compile green; zero production constructors migrated |
| T5 kernel and encoder | candidate interface | `impl/rfc0162-kernel-ops`, `impl/rfc0162-kernel-templates` | minimal opaque kernel templates; encoder accepts validated authority only; exact encoding tests | six opaque operations at `9058e272f`; visitor/opacity/rejection tests green and final Opus ACCEPT; fixed templates and authority wiring active, queue terminal pending |
| T6 mandatory production gate | pending with source-authority candidate | integration; `impl/rfc0162-checked-entry` | private `ValidatedWirModule`; no unchecked production encode/raw-body path | Glamour rewrite and adapter inputs are opaque and rechecked at `d799089dc`; final optimized WIR gate, layout contradiction proof, and raw encoder removal remain pending |
| T7 external TCB and evidence | queued Binaryen and SEC candidates; mutation active | `impl/rfc0162-binaryen-pin`; `impl/rfc0162-sec-corpus`; `impl/rfc0162-mutation-evidence` | pinned Binaryen; SEC, mutation, fuzz, poison/reuse, zero-cost, and compiler-cost artifacts | pin/release integration through `43720e967`; 15-case permanent source and exact production corpus at `ac4932bda`; final Fable/Opus verdicts ACCEPT; terminal merge, release-matrix CI, forged-WIR SEC negatives, mutation, fuzz, and zero-cost evidence pending |
| T8 review and closeout | pending | integration | Fable review, Opus adjudication if rejected, resolved findings, terminal merge audit | pending |

## Acceptance ledger

`pending` means the required evidence is absent or contradicted by current
code. `candidate` means reusable work exists but is not landed proof.

| # | Contract | State | Current evidence / contradiction | Evidence required to close |
|---:|---|---|---|---|
| 1 | Production codegen has one checked source entry | pending | `d799089dc` binds the compiler-generated Glamour rewrite to its exact checked source, but public encoders still accept ordinary `WirModule` and exact source-layout contradictions are not checked | Test-only raw entrypoints; contradictory or missing source layouts reject |
| 2 | SafeWIR carries object, layout, extent, provenance, ownership, and generation | pending | No complete fact carrier exists | Accepted and forged fixtures for every fact family |
| 3 | RichWasm correspondence is explicit | candidate | `0162-safewir-judgment.md` names the RichWasm sections and Witchy extensions; terminal review/merge pending | Every core rule maps to RichWasm or a reviewed named Witchy extension |
| 4 | Independent normative judgment | candidate | Schema-1 judgment and 20 stable IDs exist on queued `3189cb910`; the rewritten report validator partitions enforced and explicitly deferred rules, but has no complete mutation assurance and does not implement joins/fixpoints | Stable local, transfer, join, and fixpoint rule IDs; accepted/rejected/mutation fixtures per rule |
| 5 | Call results default to possible aliasing | candidate | SEC-078 landed as `da098299a` with source, pre-fix-red, differential, RC-floor, isolated-gate, and Opus evidence | Cover direct, indirect, recursive, closure, projection, imported-call, and validator matrices |
| 6 | Final optimized WIR checked once and fail closed | pending | No `ValidatedWirModule`; encoding is directly callable | Private validated constructor and production encoder API negative tests |
| 7 | Optimizer combinations cannot retain stale facts | candidate | `030af4e6b` binds indexed-kernel range facts to definition, object, generation, index, and exact length source, with reassignment/join/loop invalidation negatives; whole-pipeline combinations remain incomplete | Reassignment, join, loop, inline, specialize, recursion, and pass-combination rejection fixtures |
| 8 | Raw memory is sealed | pending | All-crates structural parser confirms 584 sites in 25 files; no-growth ceiling is locally green but migration remains zero of 584 | Current census reaches zero outside the named kernel-template module; structural test blocks regressions |
| 9 | Kernel is small and directly tested | candidate | Six opaque operation shapes at `9058e272f` cannot carry ordinary WIR bodies and ordinary encoding rejects them; production helpers remain unmigrated and fixed templates are not yet integrated | Published operation census/contracts; poison, boundary, reuse, exact-template, and mutation tests |
| 10 | Encoder mechanically erases facts | pending | Encoder accepts arbitrary WIR and supports raw bodies | Exact template tests, mandatory Wasm validation, and no production raw-body route |
| 11 | Applicable SEC corpus is permanent | candidate | `ac4932bda` restores distinct SEC-036 and pins SEC-037/038/039 source plus exact 15-case production-Binaryen evidence; SEC-078 is landed on master; forged-WIR and encoder-template negatives remain pending | Source regression plus forged-WIR rejection for SEC-036/037/038/039/078; encoder test where applicable |
| 12 | Validator mutations are killed | pending | Validator absent | Mutation matrix kills every rule deletion, inversion, or weakening |
| 13 | Binaryen trust is explicit and reproducible | candidate | `43720e967` supplies the PATH-free digest-pinned version-131 implementation, release bundle installation, exact recipe/cache identity and fallback tests, `PreserveRaw` exclusion, authenticated official/package/install smokes, reviewed-production performance lane, and a 13-case exact pre/post production corpus; terminal merge and release-matrix CI evidence are pending | Pinned executable identity/recipe/cache key, explicit fallback, `PreserveRaw` exclusion, pre/post differential corpus |
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
