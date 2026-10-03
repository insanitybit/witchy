# Witchy design contracts

The working set is intentionally small. A design status does not prove that
current source implements it; use the cited ledgers, current tests, and exact
merge-queue result for that claim.

| Document | Purpose |
|---|---|
| [RFC-0167](0167-witchy-language-contract.md) | Sole target source-language contract; normal mode is the current delivery scope and opt mode is deferred. |
| [RFC-0167 ledger](0167-acceptance-ledger.md) | Normal-mode implementation and evidence obligations. |
| [RFC-0126](0126-capability-effects-contract.md) | Capability and effect contract. |
| [RFC-0165](0165-safewir-canonical-ir.md) | Canonical SafeWIR compiler architecture. |
| [RFC-0165 ledger](0165-acceptance-ledger.md) | Bounded implementation evidence and remaining safety limits. |
| [SafeWIR judgment](0162-safewir-judgment.md) | Independent schema-1 memory-validation rules. |
| [RFC-0163](0163-authority-validated-codegen.md) | Proposed stronger codegen and host-authority validation. |
| [Current work](current-work.md) | Priority order, retained performance obligations, and parked proposals. |
| [Historical recovery](retired-rfcs.md) | Git retrieval and current authority for removed RFCs. |

New design work should amend an existing contract when it changes its rules.
Add a separate numbered RFC only for a scheduled mechanism with distinct,
verifiable acceptance criteria. Numbers retired from the working tree are not
reused. Machine-readable evidence fixtures may remain under `rfcs/` even when
their historical prose body is removed.

Lifecycle labels for numbered RFCs are `proposed`, `accepted`, `planned`,
`deferred`, `implemented`, `rejected`, and `superseded`. A proposal is not an
implementation commitment. `implemented` applies only to its stated boundary;
it never automatically closes another RFC or the current release gate.
