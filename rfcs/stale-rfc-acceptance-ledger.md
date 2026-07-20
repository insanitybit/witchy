# Stale RFC terminal-outcome ledger

This ledger records the current-master evidence and terminal disposition work
for RFCs whose implementation or status outlived the agent session that owned
it. It is intentionally stricter than an `in progress` note: every row must end
as `PROVEN`, `MISSING`, `FAILING`, `EXTERNALLY BLOCKED`, or `EXPLICITLY
DEFERRED`, and every non-terminal implementation slice must be discoverable as
`QUEUED`, `BLOCKED`, or `PICKUP` with a branch and commit.

Evidence was refreshed from master `89b47e87` and the merge-queue journal on
2026-07-19. Queue and branch facts are snapshots; the individual RFC tracking
field is updated when a terminal decision lands.

## Terminal-outcome matrix

| Item | Requirement | State | Evidence / terminal action |
|---|---|---|---|
| RFC-0066 | Operation-bound WebAuthn assertions protect promote and yank | PROVEN | `projects/coven-web/src/coven_web.witchy` verifies fresh single-use assertions; `projects/coven-web/verify.py` covers valid, replayed, cross-operation, and tampered assertions. |
| RFC-0066 | Registration verifies the WebAuthn creation ceremony, origin, RP, and counter | MISSING | `h_wa_register` currently persists client-supplied `credentialId` and `publicKey` after only non-empty checks. |
| RFC-0066 | Glamour slot dispatch requires authority for the selected slot kind | MISSING | The RFC specifies a token, but `glamour.slot(kind, data)` and the committed host test carry no slot authority. |
| RFC-0070 D6 | Checked-module seam prevents selected production compilers from omitting type checking | PROVEN | Merged as `89b47e87`; `CheckedModule` and shared `link_checked` entry points cover embedded PM, Coven, and the runtime compiler path. |
| RFC-0070 D6 | User source is checked before destructive lowering and comptime output re-enters the same front door | MISSING | RFC-0070 explicitly records the merged seam as the first slice, not the full pipeline reorder. |
| RFC-0077 | Test-only sealed data, zero-grant unit tests, mock Dir, explicit Dir/Net integration grants, and deterministic collaborators | PROVEN | Master contains linker/runner enforcement, compiled and interpreter mock-Dir support, integration-grant tests, `testing.fixed_clock` / `fixed_rand`, and the testing book chapter. |
| RFC-0077 | Future capability mocks remain part of the accepted RFC | EXPLICITLY DEFERRED | Close the delivered RFC scope; any mock Net/Env or capability-valued Clock/Rand is a new proposal with its own backend and denial evidence. |
| RFC-0079 | Durable unit state survives an agent session and cannot disappear as chat context | MISSING | The proposed `scripts/agent-queue.sh` and unit-loop workflow do not exist; the RFC's gitignored scratch layout is not a durable branch handoff. |
| RFC-0079 | Smallest useful enforcement detects dropped RFC work | MISSING | Implement a report/lint surface for clean-ahead unqueued branches, dirty RFC worktrees without handoff, and vague non-terminal RFC metadata. |
| RFC-0080 | Structured quotation/builders and definition/call-site identity implemented to the recorded boundary | PROVEN | Current RFC tracking and `tests/rfc0080/` record the landed compiler-owned syntax categories and hygiene behavior. |
| RFC-0080 | Qualified identity residual | EXTERNALLY BLOCKED | `impl/rfc0080-qualified-identities` was actively gating when this ledger snapshot was created; resolve from its terminal journal event, not by duplicating it. |
| RFC-0080 | Persistent per-node origin slice | MISSING | A clean historical patch exists, but its branch is hundreds of commits behind and must be transplanted onto fresh master after overlapping RFC-0080 work lands. |
| RFC-0080 | Remaining compatibility-builder origins and item/field identities | EXPLICITLY DEFERRED | Keep only separately scoped, acceptance-tested follow-ups; do not retain a general `proposed` tail after the current slices settle. |
| RFC-0082 | Backend-neutral canonical runtime type identity and descriptor plan | PROVEN | `impl/rfc0082-descriptor-catalog` merged in the green batch recorded at master `89b47e87`; the plan authenticates package/declaration identity, rejects capability types, and is closed over nested types. |
| RFC-0082 | Source `Dynamic`, payload conversion, checked field/call operations, parity, tooling, and documentation | EXPLICITLY DEFERRED | These are post-0.1 stages and remain excluded from `RELEASE-READINESS.md`; revive only after the descriptor foundation lands and authenticated loader ownership is available. |
| Implementation roadmap | Historical capability/identity sequence no longer claims active work | MISSING | It still says RFC-0011 is partial even though RFC-0011 and its tested policy surface are implemented. Freeze as historical. |
| RFC execution plan | Operational pickup index matches current RFC truth | MISSING | It names RFC-0005 as the next pickup although RFC-0005 is implemented. Retire it as a live index in favor of generated/live status. |
| Coven namespaces plan | Provider-derived namespace model is implemented | EXPLICITLY DEFERRED | Current Coven implements two-segment namespace/repository binding, not the proposed provider/owner/name and declarative multi-provider model. Remote Coven lifecycle is excluded from 0.1. |
| RFC-0084 | Scoped extensions/interception have an accepted, testable implementation contract | EXPLICITLY DEFERRED | No implementation branch or acceptance ledger; revive after RFC-0082 dynamic dispatch is implemented and the RFC gains explicit acceptance criteria. |
| RFC-0085 | Capability-bounded dynamic compilation/loading has its prerequisite loader and isolation contracts | EXPLICITLY DEFERRED | No implementation branch; revive after RFC-0080/0082 and an authenticated loader/authority-ceiling contract are implemented. |
| RFC-0086 | Native extension ABI and trust boundary are accepted for implementation | EXPLICITLY DEFERRED | No implementation branch; `trusted-exe` explicitly rejects `NativeLoader`, and this expands the trusted computing base. Revive only with a separately approved ABI/security effort. |
| RFC-0095 | Grimoire trusted application discovery/install is in the current release contract | EXPLICITLY DEFERRED | `RELEASE-READINESS.md` excludes it. Revive after 0.1 when Coven artifacts, byte-safe atomic installation, and the required trusted-exe bindings are scheduled together. |

## Goal-owned live work

This section is updated whenever a slice leaves a worktree. A row may not be
left as an unqualified `in progress` state.

| Slice | Durable state | Branch / evidence | Next action |
|---|---|---|---|
| Acceptance ledger | PICKUP | `docs/stale-rfc-terminal-ledger` at master `89b47e87` | Validate as prose-only, submit through the merge queue, and record the terminal journal event. |
| RFC-0070 D6 checked-stage seam | terminal | merged `89b47e87` | Continue the full reorder only as independently checked slices. |
| RFC-0082 descriptor foundation | terminal | merged in `89b47e87` | Source-level Dynamic stages remain explicitly deferred pending authenticated loader ownership and post-0.1 scheduling. |
| RFC-0080 qualified identities | QUEUED | `impl/rfc0080-qualified-identities` at submission `11664c11` | Observe its terminal journal event before editing overlapping RFC-0080 tracking. |

## Completion rule

This ledger is complete only when every `MISSING`, `FAILING`, and `EXTERNALLY
BLOCKED` row has become `PROVEN` or `EXPLICITLY DEFERRED` with a concrete revival
condition; every scoped RFC or plan has terminal metadata; and every goal-owned
branch has a terminal merge-queue journal event or a checked-in pickup record.
