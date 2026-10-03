# Implemented decisions consolidated on 2026-10-03

This is a navigation record, not an acceptance certificate for current `master`.
The 20 RFC bodies listed below declared their own boundaries implemented and had
no direct Markdown references when consolidated. Git retains their full designs
at `d31b2e4cb` (`git show d31b2e4cb:rfcs/0046-typed-trait-dispatch.md`).
The current specification, source, and tests decide executable behavior;
[RFC-0167's ledger](0167-acceptance-ledger.md) and the SafeWIR ledgers retain
open acceptance obligations. A historical `implemented` status does not prove
those wider contracts.

| RFC | Retained decision | Current home |
|---|---|---|
| 0046 typed trait dispatch | Resolved trait dispatch uses structured types rather than rendered names. | `crates/witchy-types/src/traits/mono.rs`; [language contract](0167-witchy-language-contract.md) |
| 0062 closure escape elision | Non-escaping closures may avoid heap allocation; escaping closures retain ownership. | `crates/witchy-lower/`; [performance spec](../spec/performance.md) |
| 0072 diagnostic goldens | Error messages and traps have regression fixtures. | `tests/`; [language spec](../spec/language.md) |
| 0073 one home per compiler fact | Shared layout and type facts replace duplicate private tables. | `crates/witchy-wir/src/layout/`; [architecture](../spec/architecture.md) |
| 0075 lead with the differentiator | Capability vocabulary and first-contact positioning are in the public docs. | [README](../README.md); [capabilities spec](../spec/capabilities.md) |
| 0090 proper tail calls | Tail-call lowering preserves constant-stack semantics for admitted call forms. | [language contract](0167-witchy-language-contract.md); `crates/witchy-lower/` |
| 0093 diff-scoped merge gate | The coordinator selects a gate from the branch diff. | [agent instructions](../CLAUDE.md); `scripts/merge-queue.sh` |
| 0094 persistent test-compile caching | Exact-identity cached standard-library expansion may accelerate tests. | `crates/witchy-syntax/`; `scripts/` |
| 0101 source-first compiler pipeline | Checked source precedes destructive lowering on the production route. | [architecture](../spec/architecture.md); `crates/witchy-types/src/pipeline/` |
| 0105 deterministic capability fixtures | Test plans and transcripts use a shared capability fixture host. | `crates/witchy-testkit/`; `crates/witchy-test-host/` |
| 0115 agent contributor velocity | Fast checks and agent workflow have one operational guide. | [agent instructions](../CLAUDE.md); `scripts/agent-check.sh` |
| 0119 Coven web human 2FA issuer | The original issuer design was implemented for the then-current Coven web boundary. | `projects/coven-web/`; [product status](../PRODUCT-STATUS.md) |
| 0120 staged version installs | The package manager may opt into unreleased versions. | `projects/coven/`; [package manager design](package-manager.md) |
| 0137 compiler-derived CSP | Platform confinement derives from capability grants. | [capabilities spec](../spec/capabilities.md); `crates/witchy-runtime/` |
| 0144 hybrid list and string primitives | Compiled lists, strings, and borrowed slices use specialized representations. | `crates/witchy-wir/`; [RFC-0143 collection ledger](0143-acceptance-ledger.md) |
| 0151 owned byte and text construction | Mutable byte builders and bounded copies use checked ownership. | `std/bytes.witchy`; `crates/witchy-lower/` |
| 0158 macOS Seatbelt confinement | Seatbelt implements the derived macOS sandbox boundary. | `crates/witchy-runtime/`; [capabilities spec](../spec/capabilities.md) |
| 0159 merge-queue telemetry | Queue receipts and stage timing are recorded by the coordinator. | `scripts/merge-queue.sh`; [agent instructions](../CLAUDE.md) |
| 0170 cut WIR from types | The compiler crate dependency graph separates WIR from type checking. | `crates/witchy-types/Cargo.toml`; `crates/witchy-wir/Cargo.toml` |
| 0171 recycled worktrees | Agent worktrees keep warm incremental build state. | [agent instructions](../CLAUDE.md); `scripts/worktree-create.sh` |

These decisions may still have regression bugs or incomplete wider RFC-0167,
SafeWIR, and security acceptance. Their old bodies are historical evidence,
not active work queues. No RFC number is reused.
