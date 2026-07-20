# Stale RFC terminal-outcome ledger

This ledger records the current-master evidence and terminal disposition work
for RFCs whose implementation or status outlived the agent session that owned
it. It is intentionally stricter than an `in progress` note: every row must end
as `PROVEN`, `MISSING`, `FAILING`, `EXTERNALLY BLOCKED`, or `EXPLICITLY
DEFERRED`, and every non-terminal implementation slice must be discoverable as
`QUEUED`, `BLOCKED`, or `PICKUP` with a branch and commit.

Evidence was refreshed from master `88eab422` and the merge-queue journal on
2026-07-20. Every goal-owned slice below has a terminal green queue event; the
individual RFC tracking fields carry the durable disposition.

## Terminal-outcome matrix

| Item | Requirement | State | Evidence / terminal action |
|---|---|---|---|
| RFC-0066 | Operation-bound WebAuthn assertions protect promote and yank | PROVEN | `projects/coven-web/src/coven_web.witchy` verifies fresh single-use assertions; `projects/coven-web/verify.py` covers valid, replayed, cross-operation, and tampered assertions. |
| RFC-0066 / RFC-0099 | Registration verifies the WebAuthn creation ceremony, bootstrap authority, origin, RP, and counter | EXPLICITLY DEFERRED | `h_wa_register` currently persists client-supplied `credentialId` and `publicKey` after only non-empty checks. RFC-0099 must revive before remote Coven registration enters a supported release contract. |
| RFC-0066 / RFC-0100 | Privileged Glamour slot dispatch requires authority for the selected slot kind | EXPLICITLY DEFERRED | Current committed renderers are presentation-only. RFC-0100 must revive in the same dependency stack as the first authority-bearing renderer. |
| RFC-0070 D6 | Checked-module seam prevents selected production compilers from omitting type checking | PROVEN | Merged as `89b47e87`; `CheckedModule` and shared `link_checked` entry points cover embedded PM, Coven, and the runtime compiler path. |
| RFC-0070 D6 / RFC-0101 | User source is checked before destructive lowering and comptime output re-enters the same front door | EXPLICITLY DEFERRED | RFC-0101 owns the unimplemented source-first reorder. It must revive before any new destructive pre-check transform, after any regression where lowering erases a source-only diagnostic, or before making a fully source-first release claim. |
| RFC-0077 | Test-only sealed data, zero-grant unit tests, mock Dir, explicit Dir/Net integration grants, and deterministic collaborators | PROVEN | Master contains linker/runner enforcement, compiled and interpreter mock-Dir support, integration-grant tests, `testing.fixed_clock` / `fixed_rand`, and the testing book chapter. |
| RFC-0077 | Future capability mocks remain part of the accepted RFC | EXPLICITLY DEFERRED | Close the delivered RFC scope; any mock Net/Env or capability-valued Clock/Rand is a new proposal with its own backend and denial evidence. |
| RFC-0079 | Durable unit state survives an agent session and cannot disappear as chat context | PROVEN | `scripts/rfc-status.sh` derives state from checked-in RFC metadata, refs/worktrees, and the merge queue; `tests/worktree/rfc_status.rs` exercises stale, invalid, pickup, queued, tracked, and terminal states. Merged green as `4b06aaa1`. |
| RFC-0079 | Smallest useful enforcement detects dropped RFC work | PROVEN | `scripts/rfc-status.sh --check` rejects clean-ahead unqueued RFC branches, dirty RFC worktrees, unowned proposals/plans, vague `in-progress`, and unknown statuses. The full gate, 13-test worktree suite, and 17-test path-routing suite passed. |
| RFC-0080 | Structured quotation/builders and definition/call-site identity implemented to the recorded boundary | PROVEN | Current RFC tracking and `tests/rfc0080/` record the landed compiler-owned syntax categories and hygiene behavior. |
| RFC-0080 | Qualified identity residual | PROVEN | `impl/rfc0080-qualified-identities` merged as `11664c11`; `tests/rfc0080/qualified_identities.rs` proves definition-site and explicit call-site module-qualified type and constructor-pattern identities on both backends. |
| RFC-0080 | Persistent per-node origin slice | PROVEN | Current master records structural `GeneratedNodePath` entries through `record_item_tree`; `crates/witchy-syntax/src/origin.rs` and the comptime integration assertion in `crates/witchy-interp/src/comptime.rs` prove lookup and nested ancestry. The stale historical branch is superseded. |
| RFC-0080 | Capability type builder preserves structural rights | PROVEN | `impl/rfc0080-capability-type-current` is a fresh-master transplant of the valid code and test hunks from the preserved dirty historical worktree. The RFC-0080 dual-backend test and intrinsic privacy/catalog guards pass; the bridge validates the capability head and retains each right as a structural `Type` child. |
| RFC-0080 | Remaining source-projecting builders, item identity, `ModuleSyntax`, `Span`, and tooling | EXPLICITLY DEFERRED | RFC-0080 is terminally deferred after the implemented foundation. Revive when a scheduled compiler/library consumer requires a missing operation, or before promising a fully structural public metaprogramming API. |
| RFC-0082 | Backend-neutral canonical runtime type identity and descriptor plan | PROVEN | `impl/rfc0082-descriptor-catalog` merged in the green batch recorded at master `89b47e87`; the plan authenticates package/declaration identity, rejects capability types, and is closed over nested types. |
| RFC-0082 | Checked catalog joins retained declarations to loader ownership | PROVEN | `CheckedModule::runtime_declaration_catalog` fails closed on missing owners; focused pipeline/runtime-type tests cover authenticated resolution and the failure path. Catalog construction and mutation are crate-private, making the checked seam enforceable. |
| RFC-0082 | Source `Dynamic`, payload conversion, checked field/call operations, parity, tooling, and documentation | EXPLICITLY DEFERRED | Revive after 0.1 when the production loader transports authenticated package coordinates. No stage may reconstruct identity from import aliases, filesystem paths, compiler names, or display names. |
| Implementation roadmap | Historical capability/identity sequence no longer claims active work | PROVEN | Merged in `cb9da148`: the document is marked implemented/historical and no longer claims RFC-0011 is partial. |
| RFC execution plan | Operational pickup index matches current RFC truth | PROVEN | Merged in `cb9da148`: the document is explicitly superseded and directs readers to current RFC tracking plus the checked-in ledger. |
| Coven namespaces plan | Provider-derived namespace model is implemented | EXPLICITLY DEFERRED | Current Coven implements two-segment namespace/repository binding, not the proposed provider/owner/name and declarative multi-provider model. Remote Coven lifecycle is excluded from 0.1. |
| RFC-0084 | Scoped extensions/interception have an accepted, testable implementation contract | EXPLICITLY DEFERRED | No implementation branch or acceptance ledger; revive after RFC-0082 dynamic dispatch is implemented and the RFC gains explicit acceptance criteria. |
| RFC-0085 | Capability-bounded dynamic compilation/loading has its prerequisite loader and isolation contracts | EXPLICITLY DEFERRED | No implementation branch; revive after RFC-0080/0082 and an authenticated loader/authority-ceiling contract are implemented. |
| RFC-0086 | Native extension ABI and trust boundary are accepted for implementation | EXPLICITLY DEFERRED | No implementation branch; `trusted-exe` explicitly rejects `NativeLoader`, and this expands the trusted computing base. Revive only with a separately approved ABI/security effort. |
| RFC-0095 | Grimoire trusted application discovery/install is in the current release contract | EXPLICITLY DEFERRED | `RELEASE-READINESS.md` excludes it. Revive after 0.1 when Coven artifacts, byte-safe atomic installation, and the required trusted-exe bindings are scheduled together. |

## Scoped terminal dispositions

| Scoped item | Terminal disposition |
|---|---|
| RFC-0066 | `implemented`; residual registration/bootstrap and authority-bearing Glamour contracts are deferred as RFC-0099 and RFC-0100. |
| RFC-0070 | `implemented` decision record; the stronger source-first reorder is deferred as RFC-0101. |
| RFC-0077 | `implemented`; future capability mocks require a new proposal. |
| RFC-0079 | `deferred`; its durable RFC-status enforcement prerequisite is implemented. |
| RFC-0080 | `deferred`; the structured metaprogramming foundation is implemented and its remaining surface has explicit revival triggers. |
| RFC-0082 | `deferred`; descriptor/catalog foundations are implemented and user-visible `Dynamic` stages have an authenticated-loader revival trigger. |
| `implementation-roadmap.md` | `implemented` historical record, not a live plan. |
| `execution-plan.md` | `superseded`; current RFC metadata plus this ledger are authoritative. |
| `coven-namespaces-plan.md` | `deferred` until remote Coven lifecycle work is scheduled. |
| RFC-0084 | `deferred` behind RFC-0082 dynamic dispatch and explicit acceptance criteria. |
| RFC-0085 | `deferred` behind the metaprogramming/dynamic stack and an authenticated loader authority ceiling. |
| RFC-0086 | `deferred` pending a separately approved native ABI and trust-boundary effort. |
| RFC-0095 | `deferred` until the post-0.1 Coven artifact/install stack is scheduled. |

## Goal-owned live work

This section is updated whenever a slice leaves a worktree. A row may not be
left as an unqualified `in progress` state.

| Slice | Durable state | Branch / evidence | Next action |
|---|---|---|---|
| Acceptance ledger | terminal | merged in `28d84402`, then updated by subsequent slices | This final closeout contains no open requirement classifications. |
| RFC-0070 D6 checked-stage seam | terminal | merged `89b47e87` | The unimplemented global reorder is explicitly deferred to RFC-0101. |
| RFC-0070 terminal split | terminal | merged `45b1fc62` | No remaining action until RFC-0101's revival trigger fires. |
| RFC-0082 descriptor foundation | terminal | merged in `89b47e87` | Retained by RFC-0082's explicit deferred status. |
| RFC-0080 qualified identities | terminal | merged `11664c11` | No remaining action. |
| RFC-0066 terminal split | terminal | merged `aa27f34b`; residual security contracts are RFC-0099 and RFC-0100 | Revive those deferred RFCs only at their recorded product-boundary triggers. |
| RFC-0079 durable status | terminal | merged `4b06aaa1`; full gate and queue-infrastructure shard green | Use `scripts/rfc-status.sh --check` as the durable pickup/staleness guard. |
| RFC-0080 structural capability type | terminal | merged with the RFC-0082 seam at `3a1ec9f2`; full gate green | No remaining implementation action. |
| RFC-0082 checked catalog seam | terminal | merged with RFC-0080 at `3a1ec9f2`; full gate green | Catalog construction hardening remains queued below. |
| RFC-0082 catalog construction boundary | terminal | submitted as `ccf7dbf0`, merged green at `996a08f9` | No remaining action until RFC-0082's revival trigger fires. |
| RFC-0080 terminal metadata | terminal | submitted as `efce3fe2`, merged docs-only at `88eab422` | No remaining action until RFC-0080's revival trigger fires. |
| RFC-0082 terminal metadata | terminal | submitted as `29fa784c`, merged green with its boundary at `996a08f9` | No remaining action until RFC-0082's revival trigger fires. |

## Completion rule

This ledger is complete only when every `MISSING`, `FAILING`, and `EXTERNALLY
BLOCKED` row has become `PROVEN` or `EXPLICITLY DEFERRED` with a concrete revival
condition; every scoped RFC or plan has terminal metadata; and every goal-owned
branch has a terminal merge-queue journal event or a checked-in pickup record.
