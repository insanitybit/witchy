# Design decisions

Start with [RFC-0167: Witchy syntax and semantics](0167-witchy-language-contract.md).
It is the single target language contract, replacing the separate language
capstones. Its choices are explicit; its status is **proposed**, not implemented.
The [acceptance ledger](0167-acceptance-ledger.md) records the remaining adoption
and compiler work. Optimization expansion is deferred.

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
[Stable semantics converge before support](0135-stable-semantics-converge.md)
continues to govern parity. Their security checks and evidence obligations
are not weakened by the source-language consolidation.

## Consolidation policy

At the user's request, the 2026-09-16 cleanup removes 54 obsolete language RFC
and roadmap bodies instead of leaving conflicting proposals beside the new
contract. [The retirement index](retired-rfcs.md) maps every removed file to its
replacement and names the Git revision that recovers its exact old contents.
No retired RFC number is reused.

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
