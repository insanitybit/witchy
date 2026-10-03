# Design decisions

Start with [RFC-0167: Witchy syntax and semantics](0167-witchy-language-contract.md).
It is the single target language contract, replacing the separate language
capstones. Its choices are **accepted**, not fully implemented. The
[acceptance ledger](0167-acceptance-ledger.md) records the remaining compiler
and evidence work. Opt-mode acceptance is deferred; safe, general normal-mode
optimization remains in scope.

## Current focus (2026-10-03)

These are work-selection priorities, not completion claims. Recheck each ledger
and the current compiler route before closing a row.

| Priority | Decision and next evidence |
|---|---|
| 1. Normal-mode language and tooling | Close the remaining [RFC-0167 ledger](0167-acceptance-ledger.md) rows with admitted Wasm behavior, source-level expected results, and safety refusals. The accepted contract is not a completion certificate. |
| 2. Verified compiler output | Reconcile [canonical SafeWIR](0165-safewir-canonical-ir.md), the [object-memory judgment](0162-validated-safewir.md), and [authority validation](0163-authority-validated-codegen.md) against the current executable route. For each optimization, prove its specific behavior-preserving rewrite and recheck the final memory and authority graph. Their older ledgers are evidence inputs, not current-tip certification. |
| 3. Measured runtime gaps | Use [RFC-0153's effect-safe loop work](0153-effect-safe-range-and-loop-optimization.md) and [RFC-0146's measured program](0146-post-hybrid-performance-program.md) only where matched normal-mode workloads and profiles still show a gap. Preserve [RFC-0143's collection ledger](0143-acceptance-ledger.md) for any remaining representation obligation. Promote generic changes with correctness and performance evidence. |
| 4. Compile-time cost | Profile before selecting among [arena/interning](0164-compiler-performance-arenas-and-interning.md), [compile std once](0169-compile-std-once.md), and the [protected service](0172-protected-compiler-service.md). These are proposals, not three simultaneous commitments. |

The Glamour, hosted Coven, native-extension, and deferred opt-mode proposals
remain parked at their stated triggers. Implemented RFCs remain historical
decision records. The [retirement index](retired-rfcs.md) removes superseded
and rejected bodies from the working corpus without deleting Git history or
closing surviving security and runtime obligations.

## Authority

- The language contract says what syntax and semantics we intend to support.
- Focused compiler/runtime, capability/security, platform, and tooling RFCs
  retain their detailed mechanisms and outstanding acceptance obligations.
- Companion ledgers record evidence against named revisions. A historical
  PROVEN label does not certify a later compiler route.
- `spec/`, current source, and independent expected-result tests describe
  executable behavior today. A design document is not evidence that it runs.

For security detail, begin with [capabilities/effects](0126-capability-effects-contract.md).
For production compiler admission, begin with [canonical SafeWIR](0165-safewir-canonical-ir.md).
[RFC-0172](0172-protected-compiler-service.md) proposes a protected local compiler
service with warm in-memory state and shared compilation work. It is not yet an
adopted cache policy or an implemented compiler path.
[Stable semantics converge before support](0135-stable-semantics-converge.md)
continues to govern parity. Their security checks and evidence obligations
are not weakened by the source-language consolidation.

## Consolidation policy

At the user's request, the 2026-09-16 cleanup removes 55 obsolete language RFC
and roadmap bodies instead of leaving conflicting proposals beside the new
contract. [The retirement index](retired-rfcs.md) maps every removed file to its
replacement and names the Git revision that recovers its exact old contents.
No retired RFC number is reused.

[The retained-contract audit](retained-contract-audit.md) identifies remaining
historical syntax, scoped supersessions, and deferred competing proposals.
Retained examples and old acceptance labels describe their recorded revisions;
they do not restore retired syntax or override RFC-0167. Preserve their evidence,
but migrate fixtures to the selected contract before using them for new acceptance.

This explicitly amends RFC-0001's frozen-file policy: superseded bodies may be
deleted during an authorized consolidation once their surviving decisions,
active obligations, evidence, and links are accounted for. Git is the historical
archive; the working directory is the usable design set. Preserve machine-readable
evidence that tests consume. Never manufacture completion by deleting its ledger.

For later language changes, amend this contract with a dated decision and update
its acceptance ledger. Detailed RFCs proposing a mechanism must identify the
exact contract sections they would amend. Keep one home per source-language
rule. Source comments and test names may retain old IDs as historical identifiers;
resolve them through the retirement index.

## Status lifecycle

`proposed -> accepted -> planned -> implemented`

- **proposed**: reviewable design; no implied adoption or implementation.
- **accepted**: adopted direction; `tracking:` states remaining work.
- **planned**: accepted and scheduled for implementation.
- **implemented**: the complete declared acceptance boundary is proven.
- **deferred**: parked with a recorded reason and revisit condition.
- **rejected**: declined; may be archived in a later consolidation.
- **superseded**: replaced; `superseded-by:` names the successor, or the body is
  removed with a retirement-index entry.

Companion ledgers, migration/performance reports, judgments, and review notes
are not independently ratified RFCs. Evidence may retain an old numeric ID
without keeping the old design body normative.

## Naming

Use `NNNN-slug.md` and [TEMPLATE.md](TEMPLATE.md). Allocate a fresh number;
never reuse retired ones. 0096 and 0141 remain unwritten historical gaps;
0124 was folded into the expression-boundary design and is not reused.
RFC-0142's unwritten constraint-solving dependency remains outstanding.
