# witchy supported-preview issue ledger

A project's bug tracker will tell you everything that's ever been wrong with it.
That's the wrong question for someone deciding whether to depend on the thing.

This ledger answers a narrow one instead: what known defect, accepted
limitation, or missing evidence affects the journey witchy's
[supported-preview contract](PRODUCT-STATUS.md) asks new users to rely on?

Evidence snapshot: `9abf6debde7edae41a3f1db254a2e4f5dd2b0d1e`. Re-verify an
entry when its evidence has likely moved.

The local `bugs/` and `security-eval/` trees are engineering intake. They hold
sensitive reproductions, historical findings, duplicate identifiers, and stale
status prose, so their raw OPEN/FIXED counts aren't product status. A finding
from either tree becomes product truth here only after somebody checks it
against current master and classifies it against the supported-preview
boundary.

That separation is not a place to hide things. A shared compiler, runtime, or
host defect belongs here even when an experimental feature is what turned it
up.

## Status rules

- **BLOCKER** - the supported-preview claim is false or unsafe. Fix it or demote
  the affected surface before presenting the preview.
- **OPEN** - a real gap with an explicit disposition. It may be non-blocking
  only when its severity and product effect are stated here.
- **ACCEPTED** - a known residual limitation whose boundary is documented and
  whose risk is accepted for the preview. Acceptance is not a claim that the
  behavior is correct.
- **RESOLVED** - executable evidence or a narrowly reviewable documentation
  change closed the issue on a recorded commit.

Any known HIGH or CRITICAL defect in the supported journey is a blocker. A
failure involving a shared compiler/runtime boundary is evaluated by its effect
on the supported journey, not by the maturity label of the test that found it.

## Current issues

| ID | Severity | Status | Affected promise | Evidence and disposition |
|---|---|---|---|---|
| PREVIEW-001 | LOW | ACCEPTED | `Dir`/`File` confinement | Filesystem operations reject lexical, absolute, and already-present symlink escapes, but the current [canonicalize-then-open sequence](crates/witchy-runtime/src/confine.rs) is not race-free against concurrent local symlink replacement. Keep this limitation explicit until operations use a syscall-level beneath/no-follow mechanism or an equivalent preopen substrate. |
| PREVIEW-002 | LOW | OPEN | First-hour CLI discovery | `witchy --help` presents supported and experimental commands in one undifferentiated list. The commands work, but the help needs the same maturity boundary as the README and `PRODUCT-STATUS.md`. This is a presentation gap, not evidence of a compiler failure. |
| PREVIEW-003 | EVIDENCE | OPEN | Unassisted first-hour use | No recorded trial yet shows that a person unfamiliar with the implementation can complete the supported journey without author help. Run a clean-machine trial, record friction and exact commands, and promote only claims the participant completed. |
| PREVIEW-004 | EVIDENCE | OPEN | Performance expectations | No tracked budget yet covers compile, link, startup, and representative execution time for the curated examples. Record repeatable native measurements and regression thresholds before making performance claims. |

## Outside this product gate

Multi-package registry workflows, Coven, Coven Web, Glamour, the browser apps,
the LSP/editor integrations, and language surfaces marked experimental or
proposed in `PRODUCT-STATUS.md` are not promoted by this ledger. They may remain
half-finished and may continue developing. Their defects become preview
blockers only when they invalidate a shared supported boundary or are exposed
as part of the supported first-user journey.

This is not permission to describe those systems as complete. Their own docs
must label them experimental and avoid availability, security-audit, or
compatibility claims that their evidence does not support.

## Updating the ledger

Every new entry should record:

1. a stable `PREVIEW-NNN` identifier;
2. severity and one status from this file;
3. the exact affected supported promise;
4. an exact commit and deterministic command or source-level proof;
5. expected and actual behavior, including the affected platform; and
6. a fix, demotion, acceptance, or evidence-gathering disposition.

Do not close entries from RFC status, grep counts, test volume, or a branch-only
fix. Update the evidence snapshot only after the relevant change is on master.
