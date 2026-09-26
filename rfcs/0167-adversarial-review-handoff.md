# RFC-0167 adversarial review handoff

Date: 2026-09-26.
Authority: [RFC-0167](0167-witchy-language-contract.md), especially sections 6,
7, 11, and 12. This handoff is an implementation guide, not a competing language
contract. The [acceptance ledger](0167-acceptance-ledger.md) remains the evidence
and closeout record. Documentation changes close no implementation rows.

## Task handoff

- branch: `docs/rfc0167-adversarial-contract`
- scope: spec; language contract and implementation handoff only
- touched files:
  - `rfcs/0167-witchy-language-contract.md`
  - `rfcs/0167-adversarial-review-handoff.md`
- boundaries: normal/opt calls, imported types, callable/task provenance,
  resource disposition, and prerequisites for a future lifecycle amendment
- verification commands for this documentation branch:
  - `git diff --check`
  - `./scripts/agent-check.sh edit` after committing the exact submission tip
  - merge through `./scripts/merge-queue.sh submit <branch>`; the coordinator
    owns the full gate and its journal records the outcome
- implementation/runtime evidence: none added by this documentation branch;
  refresh the current ledger and tests before claiming behavior is supported

## Instruction for the implementation model

Continue implementing RFC-0167 against its refreshed contract. Preserve ordinary
snapshots, retained immutable views, `var` write-back, declaration-owned execution
modes, and the currently selected replayable Task and quiescent closure semantics.
Apply the new clarification when auditing resource loss and mixed-mode boundaries.
Do not interpret the preceding design discussion as approval to make every Task
single-use/must-start or to adopt RFC-0161 wholesale. Section 12 identifies the
additional language decisions required for a replacement.

For each relevant operation, distinguish:

1. normal code calling normal code;
2. opt code with opt-compatible dependencies;
3. normal code calling opt code, including indirect and trait calls; and
4. normal code using values whose types are declared in opt modules.

Mode belongs to executable behavior. Stored qualifiers, obligations, authority,
layout, and actual callable provenance survive every context. Normal preparation
can repair eligible data aliases; it cannot repair resource duplication,
consumed bindings, borrowed escapes, or missing behavior provenance.

## Assumptions reviewed and resulting direction

| ID | Assumption challenged | Required direction |
|---|---|---|
| A01 | A coherent snapshot core proves all compositions. | Keep it; check nested aliases and live views. Root uniqueness is not deep uniqueness. |
| A02 | Resource safety requires replacing every replayable Task. | Compare a universal owned Task with a separate owned operation; do not silently migrate. |
| A03 | Single-use implies must-start. | Separate affinity, disposition, and completion. Design cold abandonment and capture obligations explicitly. |
| A04 | Factories mechanically preserve replay. | Preserve input evaluation time, effects, snapshots, mode, and invocation count. |
| A05 | A must Task wrapper authorizes resource closures. | Require checked resource storage and consuming paths; ordinary opaque callables cannot hide must values. |
| A06 | Structured scopes dispose of all resources. | Include cold operations and completed child results on sibling-error and parent-exit paths. |
| A07 | Cooperative cancellation guarantees cleanup. | State observation and liveness limits; preserve cleanup mode and authority. |
| A08 | Completion-only join is a local implementation improvement. | Treat it as a scheduling change affecting blocking, exits, write-back, and tail calls. |
| A09 | Closing a channel disposes of its queue. | Identify the accountable owner of residual messages, including with no receiver. |
| A10 | Returning an unsent payload restores the sender. | Define commitment and explicit recovery; preserve exactly-once transfer and argument effects. |
| A11 | Deadlock is safely reportable after dropping frames. | Preserve obligations on recoverable failure or select an explicit program-abort boundary. |
| A12 | Adding close completes resource iteration. | Define early-exit remainder ownership and synchronous/asynchronous disposition. |
| A13 | Recursive resource classification solves every discard. | Combine stored-field classification with control-flow transfer and checked consuming operations. |
| A14 | Resource-root view restrictions are cheap for applications. | Keep the safety rule; measure API and materialization costs with resource-plus-data examples. |
| A15 | Opt-module callback factories settle usability. | Keep checked provenance; evaluate dependency migration and ordinary higher-order library examples. |
| A16 | A normal caller can repair any opt incompatibility. | Restrict repair to lawful data preparation at the actual normal call site. |
| A17 | Opt-defined types authenticate stored behavior. | Preserve actual callback/task/witness proofs; module origin is insufficient. |
| A18 | Changing Task is a small runtime patch. | Account for aggregate must propagation, views, closures, errors, collections, and exported metadata. |

## Work that can proceed under the current contract

1. Audit recursive discard and transfer paths: `??`, `?`, explicit discard,
   overwrites, patterns, early returns, generic wrappers, and partial aggregate
   consumption. A reachable must-bearing error cannot disappear. Copying or an
   unexecuted cleanup callback is not disposition.
2. Trace each consuming deferred operation from its source owner through checked
   source facts, stored frame/callback state, canonical lowering, and runtime
   completion or cancellation. Converting a resource to raw state cannot erase
   its obligation. Refuse an unsupported safe path rather than admitting a leak.
3. Preserve declaration mode and authenticated behavior through normal callers,
   aggregate fields, updates, branches, aliases, generics, trait witnesses, task
   resumes, and scheduler adapters. Recheck current source before adding work:
   the implementation agent is actively improving these paths.
4. Test normal use of opt-defined plain records separately from calls to their
   opt methods. Add qualified fields, resources, callbacks, sealed construction,
   and admitted packed/view carriers. Do not infer behavior from a nominal type.
5. Test nested ownership preparation: preserve the old snapshot while a normal
   caller establishes an opt callee's actual child-storage preconditions; reject
   an opt caller lacking that proof or explicit materialization.

Use shared stored-type judgments and dataflow, not per-library-name exemptions.
No new source interpreter semantics or optional-optimizer rescue is permitted.

## Decisions required before a lifecycle replacement

Keep these design-dependent tracks separate from fixes required by today's rules.
Freeze the public state transitions and exported obligations before changing
runtime frames or standard-library signatures:

```text
select replay/owned-operation surface and cold disposition
  -> checked capture/frame/result obligations and exported contracts
  -> scope exit, cancellation, deadlock, and completed-result ownership
  -> channel close/select recovery and generator early termination
  -> standard-library migration and cross-mode acceptance evidence
```

Shared ownership and mode contracts constrain every stage; they are not deferred
until integration. Independent current-contract checker, carrier, runtime, and
evidence work can continue. Implementing a future API behind an unchanged public
Task type is not an acceptable shortcut.

## Required counterexamples and acceptance witnesses

Exercise the applicable contexts above. For future APIs, first select their
contract in the RFC; these witnesses do not themselves introduce syntax.

| Witness | Expected invariant or design answer |
|---|---|
| Unstarted data-only operation versus one owning a Ticket | Lawful cold disposition has explicit semantics; resource abandonment cannot be silent, and body effects do not run as an implicit repair. |
| Normal capture of aliased data for opt work | Capture-time values and effects remain fixed; each execution has the required actual storage proof. |
| Normal factory returning opt work | The returned work's mode does not upgrade the factory's callable contract. |
| Opt-defined callback holder supplied/updated by normal code | Construction or update cannot invent provenance; the invoked callback must satisfy the actual opt boundary. |
| Child completes with a resource before sibling failure | Parent `?` accounts for the completed result and every outstanding child/operation. |
| Cancellation races send commitment | Exactly one owner receives the payload; argument effects run once. |
| Channel closes with resources buffered and no receiver | Identify who owns and eventually disposes of or transfers the residual messages. |
| Generator stops early with suspending disposition | Specify which context can perform cleanup and reject a context that cannot. |
| Deadlocked execution owns resources | Recoverable reporting preserves obligations; a program abort is labeled separately. |
| `Result(Int, Ticket) ?? 0` versus a resource success result | Reject dropped resource errors; allow checked result transfer while preserving the resulting obligation. |
| Source-linked and separately compiled transport | Missing/tampered contracts refuse; nominal identity or printed function/result types cannot substitute for proof. |
| Optional optimization on and off | Independently expected values, effects, ownership, and scheduling agree; measure costs separately, including normal preparation. |

Acceptance requires independently derived values/diagnostics/invariants,
admitted unoptimized Wasm, and focused structural/authority/ownership checks at
the affected boundary. Run required heap/retain audits. A checked source fixture,
stale binary result, or generic green gate alone does not close a semantic row.
Unsupported paths stay named in the ledger with the exact rejection boundary.

## Reporting back

Report the branch and exact revision, owned files, selected contract, expected
result or diagnostic, commands actually run, and remaining gaps. Distinguish a
source-checking result from admitted execution and resource/heap evidence. Update
the existing acceptance rows with the implementation delta; do not mark the
deferred lifecycle design implemented because this documentation branch landed.
