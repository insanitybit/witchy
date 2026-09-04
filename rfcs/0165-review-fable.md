---
rfc: 0165
title: "External design and security review of RFC-0165"
status: review
created: 2026-09-03
reviewer: Fable 5.1 (independent of the RFC's current author)
---

# RFC-0165 review

Reviewed the uncommitted revision on `impl/safewir-production-ready` (1,340
lines, 10,703 words; the revision that expanded my 3,739-word draft). Every
claim below that names code or a spec was checked against the tree, not
inferred from the RFC. Findings are ordered by severity. "Blocking" means the
RFC should not be accepted until the item is resolved or explicitly rejected
with rationale, per the RFC's own review rule.

## Verdict

The architecture is right and I would defend it. Three decisions in the
current text contradict either the language's normative guarantees or each
other, and one scopes the hardest known problem in the field onto the critical
path of the first milestone. Resolve those four and the rest is ordinary
tightening.

## Blocking

### B1. Tail calls are a language guarantee, not an optimization

The pass-class table lists "CFG restructuring, tail calls, inlining" as
optimizations that may ship in production only with per-instance relational
control-flow validation. RFC-0090 ("Guaranteed proper tail calls") makes
proper tail calls a **normative language semantic**, with the tail-position
catalog pinned by `tests/rfc0090/tail_positions.rs` at 300,000 transitions on
both backends. Until a CFG translation validator exists, this RFC would ship a
production compiler that violates RFC-0090: deep mutual recursion traps. That
is exactly the regression the optimizer audit found on this branch (`if`-arm
tail calls no longer eliminated), and this RFC would make it policy.

Resolution: tail position is a SafeWIR semantic, not a pass. Add a tail flag
to `Call`/`VarCall` (or a `TailCall` operation) with a mandatory lowering
correspondence row: Wasm `return_call` where the target supports it, otherwise
the RFC-0090 portable loop or dispatcher. RFC-0090's criteria on loans across a
tail edge (its dependency on RFC-0083) then become checker judgments, which
this RFC's loan algebra can express. Remove "tail calls" from the optimization
table. The `lower_direct_tail_calls` pass migrates into lowering, not into the
optimizer.

### B2. The loan algebra is a borrow checker and it is on the vertical slice

`OpenReadLoan`/`OpenWriteLoan`/`Reborrow`/`EndLoan`, escrowed owner tokens,
regions, `LoanSet` transport through aggregates, calls, returns, closures and
suspension, `LoanPhi` and `AccessPhi` with per-arm correlation, and loop
fixpoints over tagged alternatives: this is Rust-NLL-class machinery, and
Polonius exists because NLL was not enough. It is the single hardest component
in the document, and the document puts it on the path of the first milestone.

It is on that path because normal-mode lowering already emits borrows. The
reclaim reproducer this RFC adopts as its vertical slice failed on this branch
precisely because `out.push("x")` in a loop produced a `BorrowedRead` empty
projection of a solely-owned object. Under this RFC that projection is an
`OpenReadLoan` requiring an `EndLoan` at the correct CFG point inside a loop
before `Reclaim` is permitted. So the slice cannot land until the full loan
algebra and loan-end placement in lowering are complete.

But the RFC also says normal mode is copy-correct and "need not name a
lifetime." Witchy has value semantics: a normal-mode value is `Unique` or
`Shared`, and nothing at source level is a loan. Loans are what an in-place or
no-copy optimizer *introduces*, and RFC-0083/0122 explicit references are an
opt-mode feature.

Resolution: stage the kernel. **v1** is `Unique | Shared | Consumed`, `Guard`,
typed `Initialize`/`Replace`/`CloneValue`/`MoveValue`, `Share`/`Retain`/
`Release`, the `DestroyTicket`, and a `Project` that yields a `Shared`-or-
copied value rather than a loan. Normal-mode lowering emits no loan
operations; the vertical slice and the first four migration families run on
v1. **v2** adds the loan algebra, gated to opt mode and to the in-place
optimizer that needs it, with its own vertical slice. This removes the hardest
third of the checker from the first milestone and from the initial TCB without
weakening any v1 judgment. It also means the reproducer's reclaim is
authorized trivially: the object is `Unique` with no loans, which is what it
always was.

### B3. The cutover bar is unreachable as written, and contradicts the drawbacks

Migration requires cutover as one atomic candidate, eligible only with
"complete known-valid source-universe coverage" over examples, projects,
bundled std, package fixtures and generated drivers, and states "there is no
earlier cutover that intentionally rejects a previously supported known-valid
source form." The Drawbacks section says that until the census floor is high
"the compiler rejects programs it used to accept." Both cannot hold. Either
cutover accepts a coverage regression, or it is a big-bang that waits for
everything.

As written it is the big-bang, and the parallel SafeWIR-native path gets zero
production exposure for the months it takes to cover every form the current
adapter never covered either. That is the fortune-burning shape the RFC exists
to avoid.

Resolution: define "no regression" against the thing being replaced. Cutover
is eligible when every program the **frozen adapter route admits today** is
admitted by the SafeWIR-native route with parity, plus the vertical bundle.
Programs neither route admits are coverage work that continues after cutover
under the same fail-closed rule. Record the frozen route's admitted set in the
manifest at freeze time so the bar is a fixed, measured list rather than
"everything."

### B4. The per-pass bars and A16 cannot both hold at cutover

The revised pass-class table requires a per-instance `RefinementReceipt` from
a registered independent checker before **any** pass runs in production,
including peephole and unreachable-pruning. That is the right end state. It
also means production has zero optimization until translation-validation
infrastructure for each class exists. Together with B1 as written (no tail
calls) and B2 as written (no in-place until loans), production performance at
cutover drops far more than the A16 budgets permit (opt median within 1.05x,
normal within 1.10x). A16 would fail on the day of cutover, or the perf
manifest would be quietly rebaselined, which A16 forbids.

Resolution: either state that the refinement checkers for the three live
passes and the tail-call lowering are cutover prerequisites, or grant the
three existing passes a dated staged bar (rechecked by the safety checker plus
differential and generator evidence) with a hard upgrade obligation and a
`PassId`-keyed expiry that A11 enforces. Pick one in the text; do not leave
A11 and A16 to collide at the gate.

## High

### H1. `ByteCopy` will expose pointer-list copy semantics; settle them first

`ByteCopy` correctly forbids copying any range containing `Ref`, `Gc`, or
`Cap`. Today `list_concat`, `list_drop` and the slot-list templates
`SafeMemoryCopy` slot buffers wholesale, and the layout catalog interns
pointer buffers as `PointerSlot::Owned`. Copying owned pointer slots by bytes
either duplicates ownership without a retain, which is a latent double-release
or leak depending on who releases, or the model is "the list owns; element
copies are shallow" and the RFC's `Owned` naming is wrong. I did not determine
which; the point is the RFC's rule forces per-element `CloneValue` for pointer
lists and the current behavior has never been tested against that. Add to
migration step 5, before the pointer-list family: state the ownership of
pointer-list elements, write the test that distinguishes the two readings, and
fix whichever is wrong.

### H2. The reentrancy rule would forbid `vm.par_map` and `vm.serve` as read

"Canonical SafeWIR v1 rejects imports that reenter compiled code or invoke a
guest callback." `vm_par_map_run` and `vm_serve_run` take a `code_idx` and
run the callable in **separate worker Wasm instances** with transport-safe
values (`host/vm_worker.rs`); `projects/coven` uses `vm.par_map`. That is not
reentry into the calling instance, but the sentence forbids "invoke a guest
callback" without qualification. Clarify: the rule targets same-instance
reentry and same-instance callbacks; isolated worker instantiation over
transport-safe values with a checked callable contract is permitted and has
its own `ImportContract` row. Otherwise cutover rejects coven.

### H3. Establishing an extent from `Unknown` needs a judgment

An `Extent::Unknown` object "cannot be projected, loaded, replaced, or
byte-copied until an exact or counted extent is established," and the only
ways to establish one are a contract or a header load. A header load is itself
a memory access on an object whose extent is unknown. The current code
resolves this with a bootstrap place (`dict_body_count_header_read_place`) and
the observation that the catalog guarantees a minimum allocation containing
the header. Make that a rule: every counted layout declares a minimum extent
covering its header, the catalog authenticates it, and the `Load` judgment
accepts a header projection on an `Unknown`-extent object of that layout
within that minimum. Without this row the design is circular for every
counted collection.

### H4. State what the checker re-verifies from `CheckedModule`

F1 assumes source type soundness is a separate theorem, which is right. But
`CheckedSourceWitness` is minted "directly from canonical checked-module types,
access/lifetime relations, declared imports, and capability kinds," and the
checker consumes layouts and callable types as ground truth. A typeck defect
that produces an inconsistent layout is then a memory-safety defect. The RFC
should list which `CheckedModule` outputs the checker independently
re-establishes (layout consistency with declared types, arity, kind agreement
between a `Cap<K>` parameter and its import row) and which it trusts. The
current text is ambiguous, and the TCB list in §Conditional safety claim omits
typeck entirely while depending on it.

## Medium

### M1. The document specifies unbuilt systems to a precision that will not survive contact

The `SuspensionFrame` schema, `VarCall` as an atomic transaction with a sealed
`AlternativeTag` result lane, the finite-powerset contract fixpoint with nine
atom kinds, `LoanPhi`/`AccessPhi`, a seventeen-field `CandidateId`, and exact
performance percentages are all specified before any of them has been
prototyped. The RFC then states that any finding changing a decision requires
an RFC revision and a schema-version impact statement. That is the right
discipline for the authority boundary (`CandidateId`, the three witnesses,
`ArtifactBinding`, the correspondence receipt), which should be fixed now and
is well specified. It is the wrong discipline for suspension frames and `var`
transactions, which will change shape when built.

Resolution: split. RFC-0165 keeps the kernel (v1 per B2), the checker
judgments, authority and admission, the backend correspondence, and the
migration. Suspension frames, `VarCall` transactions, the loan algebra (v2),
and the performance manifest each become a dependent RFC with its own version,
referenced from 0165 by number. Each can then be revised without reopening the
core.

### M2. Language-semantics assertions belong in a language RFC

§Strings asserts `substring` clamps, `chars` yields single-scalar strings,
`string.from_code` maps invalid input to U+FFFD, and that `Bytes.to_string` is
lossy while `decode_utf8` is strict. Where these restate `spec/language.md`
and `std/`, cite the section. Where they decide something new, they are
language changes and need a language RFC; a compiler-IR RFC should not be the
place a string API's failure policy is settled. (The U+FFFD behavior matches
the current `string_from_code` helper, which writes the three-byte
replacement; I did not verify the others.)

### M3. The normal-mode repair list is a language-semantics change and is versioned correctly, but its placement is odd

The list of repairs normal mode may perform (silent re-own copy, copy-on-write
for shared mutation) is versioned by `language_semantics_revision`, which is
right. It sits inside an IR RFC. Cross-reference it from `spec/language.md`
so a language reader can find the rule that says when the compiler may insert
a copy.

## Right, and worth saying so

- **Structural affine ownership instead of an alias census.** This corrects
  my own earlier proposal, which assumed `kernel_storage` was a complete
  census without anyone having shown it. The token model makes the census
  unnecessary. Keep it (in v2 per B2).
- **Three non-overlapping witnesses, `CandidateId` with the full compilation
  context, `ArtifactBinding`, and a decoded-WasmIR digest the matcher derives
  rather than receives.** This closes SEC-128 properly and forecloses the
  "recompute one witness from another" repair.
- **`CorrespondenceReceipt` with exactly-once instruction ownership.** This is
  what makes "mechanical lowering" a checkable claim instead of a description.
  Without it the lowering is in the TCB and the RFC says so honestly.
- **`RuleId` and `PassId` registries** so that weakening a rule is a schema
  change, and **assurance records that do not remove code from the TCB**.
- **M7 realm separation and the `ByteCopy` pointer-free rule.**
- **The conditional claim stated as a theorem with named assumptions
  F1/R1/H1/W1/C1/L1.** This is the first version of any of these documents
  that says exactly what is and is not being claimed.
- **Status changed to `proposed`** and a review checklist added. Correct; my
  draft's `accepted` was premature.

## Acceptance-row notes

- A9 (vertical bundle) depends on B1 and B2: with tail calls in lowering and
  loans deferred to v2, the bundle is achievable early; as written it is not.
- A10 (source-universe manifest) is the right mechanism; B3 changes what the
  cutover reads from it.
- A11 and A16 collide per B4 until one of them is amended.
- Add a row for H3 (minimum-extent header bootstrap) and for H1 (pointer-list
  element ownership decided and tested), or fold them into A6 explicitly.

## Summary for the decision

Accept the architecture. Before implementation starts, amend the text to: make
tail calls a lowering obligation (B1); stage the kernel so normal mode emits no
loans and the loan algebra is v2 (B2); define cutover as parity with the
frozen route's admitted set (B3); and make the per-pass bars and A16
consistent (B4). Split the unbuilt subsystems into dependent RFCs (M1). The
high findings are text additions of a few lines each.

With those changes the first milestone becomes small and honest: a v1 kernel
with no borrow checker, a vertical slice that reclaims a `Unique` object with
no loans to discharge, tail calls guaranteed by construction, and a cutover
bar that is a list rather than an aspiration.

---

# Round 2 — review of the 1,618-line revision

Reviewed the uncommitted revision (12,794 words, up from 10,703). Ground-truth
checks against `spec/language.md`, RFC-0083, RFC-0090, RFC-0051 evidence, and
the current helper library where cited.

## Disposition of round-1 findings

| Finding | Status | How |
|---|---|---|
| B1 tail calls | **Resolved** | `TailCall` is a mandatory v1 terminator; `ProperTailSiteManifest` independently re-derived and bijected; digest bound into every receipt and `CandidateId`; RFC-0090 probes are a cutover gate; removed from the optimization table |
| B2 loan algebra on the critical path | **Resolved in form, reopened in substance** — see N1 | `OwnershipV1/V2`, `CollectionStateV1/V2`, `CallableContractV1/V2`, `SuspensionFrameV1/V2`; v1 registry rejects loan operations; `Project` yields a non-escaping `Place` |
| B3 cutover bar | **Resolved** | digest-bound frozen adapter-admitted baseline; "finite no-regression cutover, not a big-bang"; post-cutover gaps are a coverage ledger |
| B4 A11 vs A16 | **Resolved by choosing the strict side** — see N2 | both are joint cutover prerequisites; no dated exception; a pass without an executable refinement checker stays disabled and the candidate must still meet A16 |
| H1 pointer-list `ByteCopy` | Resolved | `MoveRange`/`CloneRange`; memcopying an owned pointer slot is invalid SafeWIR even if a legacy helper does it |
| H2 reentrancy vs `vm.par_map` | Resolved | explicit carve-out for distinct-instance workers with typed marshalling and no access to caller places or frame |
| H3 `Unknown` extent bootstrap | Resolved | `AtLeast(Size)` extent with the non-circular entry rule |
| H4 typeck trust boundary | Resolved | explicit list of what the checker re-authenticates versus assumes under F1 |
| M1 split unbuilt subsystems | **Not done**; document grew by 2,100 words — see N6 | |
| M2 language semantics in an IR RFC | Not done; carried forward | |
| M3 cross-reference repair list from `spec/` | Not done; carried forward | |

## New findings

### N1 (blocking). The `let` parameter convention is a borrow, so "normal mode emits no loans" does not hold, and materializing it destroys uniqueness

The language's parameter conventions are: default (owned, copied), `let`
(**non-escaping borrow**), `var` (write-back), `own` (callee consumes). `let`
is ordinary normal-mode syntax, not an opt-mode feature. Under this revision a
`let` parameter has exactly two lowerings: a v2 loan, which v1 rejects, or
"v1-materialized" as a `Share` plus retain/release.

Materialization is not semantically neutral for the caller. `Share` converts
`Unique` to `Shared`. After `f(xs)` with `let xs`, the caller's `xs` is
`Shared`, and every subsequent mutation is copy-on-write. That is exactly the
in-place mutation the current `*_cap` family provides, which RFC-0051 measured
as load-bearing: removing it OOM-trapped several benchmarks. So a v1 that
materializes `let` fails A16 not by a few percent but structurally, and the
RFC's own freeze classification will mark most of the baseline `v2-required`,
which by its own rule means "the first production cutover implements the
required v2 subset." B2's staging collapses back to building the borrow
checker before cutover.

The fix does not need the loan algebra. A `let` parameter is lexically scoped
to one call and cannot escape; the callee's body-derived contract already
proves non-escape (no store, capture, or return of the parameter). Add to v1:

```text
CallBorrow { object }   → a call-scoped, non-escaping read view of a Unique or
                           Shared object; valid only as a Call/VarCall/TailCall
                           argument whose CallableContractV1 parameter entry is
                           NonEscaping; the caller's ownership is unchanged on
                           return; no token, no region, no reborrow, no phi
```

Its judgment is local: the callee contract entry is `NonEscaping` (derived,
never declared), the argument is live and not `Consumed`, and the caller
performs no operation on the object between the call's argument evaluation
and its return. That last condition is trivially true for a single call
expression and is what makes this a v1 construct rather than a loan. RFC-0090
tail edges need one extra rule: a `TailCall` may not carry a `CallBorrow` of a
value the frame is about to release, which the existing "no frame obligation
remains after transfer" judgment already states.

With `CallBorrow`, v1 expresses `let` parameters, unique in-place mutation
(`Replace`/`UpdateCount`/`MoveRange` on a `Unique` value), and copy-on-write
for `Shared`. The v2-required set then genuinely reduces to RFC-0083/0122
explicit `&'a T` lifetimes and borrowed *results*, which are opt-mode features.

### N2 (blocking). A16 depends on the v1 uniqueness analysis matching the zoo, and the RFC does not say so

Even with N1 fixed, A16's opt-mode budget (median within 1.05x, suite
geometric mean within 1.02x) is met only if the v1 `is_unique` analysis proves
uniqueness at every site where today's `self_*` recognizers and `*_cap`
helpers fire. RFC-0051 is the evidence that this coverage is load-bearing.
This is the largest schedule risk in the document and it is nowhere named.

Add to the freeze gate: record, per frozen workload, which in-place fast
paths fired under the adapter route (the `__witchy_reowns` counter and the
`*_cap` call sites already exist for this). Add to A16: the v1 route's
deterministic reuse and re-own counters must match the frozen per-workload
record, not merely the aggregate time budget. Otherwise A16 can pass on a
fast machine while the design has silently lost in-place mutation.

### N3 (high). Five operations are used but absent from the canonical v1 list

A6 and A15 require every operation to have a `RuleId` and a correspondence
row, so the operation list must be exhaustive. It is not:

- `UpdateCount` — defined in the `Counted` extent paragraph, absent from the list
- `MoveRange<T>` and `CloneRange<T>` — described in prose after the list
- `TailCall` — in the checker table and milestones, absent from the list
- `Assert` — "materialized into the IR only as a `Guard` or an `Assert`"; never defined anywhere

Add all five to the v1 operations block. `Assert` needs a definition: is it a
checker-only claim the final checker must re-establish (then it is not an
operation and should not be in the IR), or a runtime trap like `Guard` (then it
is `Guard`)? I would delete `Assert` and keep `Guard` as the only materialized
form.

### N4 (high). `CandidateId` binds the compiled artifact to one host implementation build

`audited_host_implementation_manifest_digest` is now inside `CandidateId`.
A candidate is therefore not the same candidate under a different runtime
*build* with an identical ABI catalog version. Every runtime patch release
invalidates every cached and published artifact, and the same source compiled
on two machines with different runtime builds produces different candidates.
That is not what the field is protecting against. The ABI catalog version
already sits in `CandidateId` and is the compile-time binding. The audited
implementation digest belongs in the launch context (assumption L1 and the
launch receipt), where it can be verified against the runtime actually present
at instantiation. Move it.

### N5 (medium). The vertical slice needs a named pass, and only one qualifies

Step 4 requires "one enabled SafeWIR pass with before/after refinement
evidence," and the new rule requires that pass to have an executable
per-instance refinement checker. Of the live passes, only unreachable-pruning
has a trivially executable checker (removed blocks had no predecessors in the
before CFG). Peephole rewrites need a rule library. Name unreachable-pruning
as the slice's pass, or the slice is blocked on building refinement
infrastructure that the slice was meant to precede.

### N6 (medium). Review-by-expansion has reached diminishing returns

This round added `CheckedLoweringWitness`, `OptimizationHistoryWitness` and
its chain validator, `ProperTailSiteManifest` with an independent enumerator,
`AtLeast`, `MoveRange`/`CloneRange`, the audited host manifest, and a
nondeterministic observation-trace coupling formalism for the freeze
baseline. Some of these close real gaps: the history witness answers a
genuine question (safety revalidation cannot synthesize refinement history),
and `AtLeast` answers H3 exactly. But the document now names roughly twenty
artifacts, none prototyped, and each round grows it by two thousand words.

Two consequences. First, the trace-coupling machinery for A10 duplicates what
`witchy-testkit` fixture plans and the deterministic executor already provide;
reuse them as the environment domain rather than specifying a new event-stream
formalism. Second, and more important: the next review should be of the
vertical slice, not of more prose. The RFC is now precise enough to build
against and too large to keep converging by revision. I would freeze the text
at N1–N4 resolved and let the prototype generate the next findings.

### N7 (low). Witness partition for A15

Eight admission artifacts now exist. A15's TCB statement should partition them
into those load-bearing for M1–M9 (source, final-SafeWIR, authority
witnesses; correspondence and byte receipts; `ArtifactBinding`) and those
load-bearing for semantic correctness only (`CheckedLoweringWitness`,
`OptimizationHistoryWitness`). Both matter, but a reader auditing the
memory-safety claim should be able to see which five to read first.

## Confirmed right in this revision

- Tail calls as a v1 terminator with an independently re-derived manifest.
  The enumerator is a second implementation of RFC-0090's tail rules inside
  the checker; that is the intent, and it is the correct place for it.
- `OptimizationHistoryWitness`: the observation that a safety recheck of an
  arbitrary final candidate cannot manufacture refinement history is exactly
  right, and binding the chain into `CandidateId` is the right closure.
- `AtLeast` with the header-bootstrap entry rule. This is the cleanest
  resolution of H3 I can imagine.
- "Memcopying an owned pointer slot without the matching move-state update or
  clone/retain is invalid SafeWIR, even if a legacy helper happens to do so."
- The per-mode observation that `Wasm traps are terminal for the instance`
  and that a host cannot publish partial `var` write-back then report a
  recoverable failure.
- The Motivation now states plainly that A3 fails on the current branch and
  the API must be replaced, not wrapped.

## Summary for the decision

Round-1 blockers are closed. One new blocker: the `let` parameter convention
is a normal-mode borrow, and without a v1 construct for it the staging in B2 is
nominal — every program using `let` becomes `v2-required` or loses uniqueness
to materialization and fails A16. `CallBorrow` as specified in N1 is a
one-judgment construct that fixes it without the loan algebra. Add the five
missing operations to the canonical list, move the host implementation digest
out of `CandidateId`, name unreachable-pruning as the slice's pass, and record
per-workload in-place counters at freeze so A16 measures what RFC-0051 showed
matters. Then stop revising and build the slice.
