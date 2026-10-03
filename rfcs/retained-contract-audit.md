# Retained RFCs after language consolidation

Audit baseline: integration commit `d3bb34c91ed6c6cb9f19af454fdcd8f51b89c993`
(2026-09-16). This is an index of design conflicts and historical evidence,
not a compiler acceptance report. [RFC-0167](0167-witchy-language-contract.md)
is accepted; [its ledger](0167-acceptance-ledger.md) alone tracks implementation
of that selected contract. No test row is promoted by this audit.

## Scoped supersessions

These documents retain mechanisms, rationale, or evidence that must not be lost.
Their older source decisions do not form alternative language contracts.

| Retained document | Superseded or qualified claim | Surviving authority and evidence |
|---|---|---|
| [0024 facts lattice](0024-unified-facts-lattice.md) | Confinement was tied to the removed `local unique` spelling. | Section 10 retains inferred nonescape and uniqueness; the analysis lattice remains useful. |
| [0032 multi-core](0032-multi-core-execution.md) | Shared-heap Tier C, borrowed-parent nursery alternatives, and treating `own`/`frozen` as transport traits. | Section 7 selects isolated workers and reference-free suspension. Retain worker grants, deterministic schedules, and measured cooperative evidence. |
| [0088 extraction](0088-ownership-aware-extraction.md) | A non-unit mutator result could stand alone; opt proof could be `local unique`; normal fallback was described inside the operation. | Sections 1, 4, and 6 require explicit discard, one unique spelling, and normal preparation before opt entry. Retain extraction/move-out algorithms, single-search counters, and snapshot evidence. |
| [0089 FIP](retired-rfcs.md) | A recursive `own unique` signature implicitly mandates constant allocation and adds body restrictions. | Superseded by sections 2 and 10. Keep the historical kernel, independent counter/stack measurements, and fixtures; uniqueness alone makes no FIP promise. |
| [0111 specialized layouts](0111-cross-boundary-specialized-layouts.md) | Transitive opt linkage could be read as one source-mode domain or mandatory specialization from uniqueness. | Sections 6 and 10 retain module-local contracts and optional physical specialization. Keep layout identity, verified adapters, destination, and header-elision evidence. |
| [0114 must obligations](0114-must-consume-obligations.md) | Borrowed-parent task motivation, opaque must-capture exception at an `own fn` call, and `_ =` discard. | Sections 1 and 7 forbid those escapes and keep disposition visible. Preserve transaction, cancellation, and all-path evidence, but reclassify incompatible old positive fixtures during implementation. |
| [0126 capabilities/effects](0126-capability-effects-contract.md) | Exercising authority was phrased as requiring receipt of the root capability itself. | Section 7 also permits opaque delegated callbacks without exposing captured roots. All grant, confinement, and unforgeability requirements remain. |
| [0138 pure/once](0138-callable-delegation-pure-once.md) | Blanket rejection of affine once captures, mandatory explicit move for default affine inputs, and incomplete spelling of caller-mutation purity. | Sections 3 and 7 permit droppable affine captures only in once callables; forbid hidden must captures and caller-visible pure writes. Retain callable identity, delegation, and security tests. |
| [0161 typed resumable runtime](0161-typed-resumable-runtime.md) and [ledger](0161-acceptance-ledger.md) | Must-start Task, scoped Child, explicit channel-owner close, and selectable-operation syntax compete with the selected replayable/quiescent surface. | Deferred. Revival requires an explicit section 7 amendment. Pending rows, frame/queue interfaces, counters, and provenance are preserved; representation work with unchanged semantics remains independent. |
| [0165 memory-mode amendment](0165-memory-mode-boundary.md) | Initial `var unique`-only and scalar-closure limits, plus managed views, could be mistaken for the final language. | Sections 4-7 select the full source contract; initial admission limits remain fail-closed implementation gaps. No weakening of independent SafeWIR validation or capability rules. |
| [Early concurrency design](retired-rfcs.md) | One message type, `Task(m, a)`, prefix await, and lack of loop suspension. | Superseded. Git retains the historical design; later evidence is preserved in the RFC-0167 ledger. |

The normal caller's lawful alias-repair copy is part of the accepted contract,
not permission to duplicate affine resources or mutate an earlier snapshot.
A current checker that rejects this case has an implementation gap in rows E/I;
that rejection does not revise section 6. Similarly, an old FIP rejection test
cannot establish a mandatory resource promise the selected language rejects.

## Historical spelling, not a competing feature

The following retained bodies contain old builtin `Nil` examples or API prose:

- [0004 CLI](0004-self-hosted-cli.md), [0012 File](0012-file-capability.md),
  [0032 workers](0032-multi-core-execution.md), and
  [0071 dogfood](0071-idiomatic-witchy-dogfood-cut.md);
- [0086 native extensions](0086-capability-gated-native-extensions.md),
  [0107 Glamour](0107-glamour-next-generation-web-framework.md), and
  [0118 atomic directory operations](0118-atomic-dir-intrinsics.md);
- [0114 must](0114-must-consume-obligations.md),
  [0138 callables](0138-callable-delegation-pure-once.md),
  [0142 configuration](0142-structural-program-configuration-and-overload-resolution.md),
  [0148 simple program](0148-glamour-simple-program.md), and
  [0161 resumable runtime](0161-typed-resumable-runtime.md).

Read those as historical unit spellings, not a surviving alias: new source uses
`()`. Do not silently rewrite a dated fixture and claim its old evidence proves
the rewritten program. RFC-0107's `Ui` API name remains; its old naming rationale
does not restore builtin `View(T, 'a)` or prohibit nominal `View` declarations.
RFC-0142 remains a proposal, not an authorization to infer structural common
rows, extend defaults beyond closed constants/direct calls, or add `comptime
inline` syntax without an explicit RFC-0167 amendment. RFC-0148 remains a library
API proposal and cannot introduce a second builtin unit spelling.

`Nil` in [WIR design](wir-design.md) names a historical internal IR variant,
not source syntax. Historical tags such as `AccessView` similarly do not restore
the removed source type constructor. Current implementation representations are
separate from the selected source language.

## Evidence and status indexes

- [README](README.md) and [retirement index](retired-rfcs.md) now agree with
  RFC-0167's accepted status. Adoption is not implementation completion.
- [0081](0081-acceptance-ledger.md), [0087](0087-acceptance-ledger.md),
  [0098](0098-acceptance-ledger.md), [0110-0112](0110-0112-acceptance-ledger.md),
  and [0122](0122-acceptance-ledger.md) remain revision-scoped evidence after
  their design bodies were removed. In particular, old positive outer-reference
  qualifier fixtures do not override section 5's rejection matrix.
- Keep the [0087 census](0087-migration-census.tsv), migration/performance reports,
  and all SafeWIR manifests, schemas, rule tables, review records, and ledgers.
  Compiler tests consume some of these artifacts directly. No evidence is deleted.
- [The stale-RFC ledger](stale-rfc-acceptance-ledger.md) records its historical
  audit revisions, not current pickup priorities or blanket implementation
  status. Its retired source decisions resolve through the new indexes.
- Deferred security/platform proposals remain deferred with their own revival
  conditions. A source-language cleanup does not ratify their trust boundaries.

## Removed execution plan

`impl-plans/0110-execution-plan.md` was an obsolete operational instruction set,
not independent acceptance evidence. It still prescribed a local-unique escape
gate and hard-coded source locations after those constructs were removed. Its
exact body is recoverable with the baseline in [retired-rfcs.md](retired-rfcs.md).
No tracked caller reads or includes that path.

The durable inputs remain in the [0110-0112 ledger](0110-0112-acceptance-ledger.md)
and the current acceptance rows E/I/N/P:

- counters for executed boundary preparation, ownership-token repair, and
  direct-storage access, not static candidate counts;
- exact checked-call identity through cloning, monomorphization, and multiple
  calls on a line, with argument position retained;
- independent expected results across direct, indirect, generic, trait, lambda,
  and projected-place calls, including optimizer-disabled execution;
- evaluated-once places, nonoverlap, loan/escape checks, compatible layouts,
  valid final write-back, and trap/error-path evidence before direct storage;
- no inferred closure or source-mode identity based on an emitted name, and no
  optimizer toggle changing source admission or inventing a safety fact.

These are evidence obligations, not instructions to preserve an obsolete
zero-token repair mechanism or restrict all future storage to whole locals.
The plan's landed commit IDs (`5d9bc824`, `ed0159c7`, `1d5b7f67`) remain historical
inputs; none is claimed to close RFC-0167.
