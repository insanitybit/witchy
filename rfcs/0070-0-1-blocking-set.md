---
rfc: 0070
title: the 0.1 blocking set — decisions that close the coherence map
status: accepted
created: 2026-07-06
tracking: accepted decision record for the in/out calls RFC-0067 ordered; evidence base is the full
  open-RFC + 273-open-bug read of 2026-07-06 (scratch/backlog-report-2026-07-06.md)
related:
  - "0061 (mechanical release gate)"
  - "0063 (coherence gate)"
  - "0067 (ordering contract — this RFC closes its open decisions)"
  - "0005 / externref-implementation-plan (decided IN, scoped)"
  - "0054 (decided UN-DEFERRED)"
  - "0066 (decided IN)"
  - "0087 (uniform var write-back - decided IN before 0.1)"
---

# RFC-0070: the 0.1 blocking set

## Summary

RFC-0067 grouped the remaining 0.1 work into canonical models and ordered them,
but deliberately stopped short of deciding which destinations must ship versus
which may be deferred. This RFC makes those decisions. It is a decision record:
each numbered decision states the call, the evidence, and what it kills.

Two rules generate every decision below:

1. **A claim in the README must be enforced in the artifact users run, or it
   must not be in the README.** Not "documented with an asterisk" — enforced
   or absent.
2. **0.1 ships nothing twice.** One spelling per concept, achieved by
   deletion, not by documenting which duplicate is preferred.

## The evaluation that forces this

Witchy's three differentiating claims are each currently enforced in the
component users do *not* run:

1. **"Capabilities are unforgeable."** True in the interpreter. But the
   interpreter is the differential oracle; `witchy foo.witchy` runs the
   compiled backend (RFC-0004 self-hosting completed this — even `pm` now
   runs compiled, f0a5da3). In the artifact users actually execute,
   capabilities are guest `i32`s in linear memory: one ownership-analysis
   false negative is a capability-forgery primitive that wasmtime's sandbox
   does not stop (RFC-0005's own threat model). The security thesis is
   currently enforced by the backend users don't touch.
2. **"Two backends, zero silent divergence."** The parity sweep tests
   programs that run. It has no gate over the *acceptance set*: ~13 open bugs
   (BUG-214, 251, 300, 302, 305, 318, 319, 321, 335, 516, 529, 539) are
   programs `check` blesses that the compiled backend then rejects or answers
   differently. The prime directive is enforced over the wrong set.
3. **"Supply-chain-safe package manager."** ~30 open bugs show the trust
   checks fail open: infallible decoders default malformed records to
   `""`/`0`/`[]` (empty authority passes the widening gate — BUG-499, 232,
   386); fetch never binds the signed record to the requested coordinate
   (BUG-266, 277); a failed root-key refetch *repins the root key*
   (BUG-371); the human-2FA promote gate compares a string against
   `"webauthn"` (BUG-219); a *failed* self-promote still persists the
   attacker as maintainer (BUG-281).

A 0.1 that ships these as footnotes is not a smaller version of the thesis;
it is a different language wearing its clothes. The blocking set below is
chosen to make the claims true, and to delete every place where the language
currently says one thing twice.

## Decisions

### D1 — RFC-0005 is a 0.1 blocker, scoped to stages 2+3+5

**In:** all root capabilities become `externref` end-to-end (stage 2: File;
stage 3: Socket/Listener and the rest); the `i32` handle tables are deleted
(stage 5). The original minimum allowed stage 4 to remain reject-first. The
implementation subsequently exceeded that minimum: closed generic aggregates,
typed closure environments, concrete function signatures, `Option`/`Result`,
and reference-bearing `List` values now have reference-preserving GC layouts.
Reference-bearing `Dict`, open generic function ABIs, region copy-out, and
cross-instance capability callbacks remain loud check-time rejections.

Rationale: capability security is the sole differentiator, and the compiled
backend is the sole run path. Reject-first makes the stage-4 deferral loud
rather than silent, which satisfies RFC-0067's "accepted residual" bucket
honestly; forgeable root caps do not. This is the longest pole and is
**main-loop-led work** (established: this depth of change is not
agent-feasible mid-integration) — it starts first and runs in parallel with
everything else.

Consequence for the backlog: every "harden the i32 handle" bug is reclassified
temporary-or-obsolete per 0067's invalidation table; none of them are fixed
for their own sake.

### D2 — One checker, one acceptance set

`check` must decide. Every backend hard-rejection becomes a check-time
diagnostic: route the existing `reject_reason` channel back into typeck so
"typechecks" *means* "runs on both backends or fails to check on both." The
13 open parity bugs become regression tests of this mechanism, not 13
individual fixes.

New permanent gate: an **acceptance-differential** arm in the fuzzer/sweep —
generated programs that typecheck must run on both backends. This is the gate
the prime directive was always missing; without it the class regrows.

### D3 — RFC-0054 is un-deferred, in full

The RFC's gate was "ecosystem demand." The demand arrived, spelled as ~30
security bugs: fail-open decoding at trust boundaries *is* what
stringly-typed, sentinel-valued errors look like at scale (the RFC-0044
straggler cluster — BUG-463's `crypto.verify` conflating malformed-input with
bad-signature is the same disease in one function).

Ship for 0.1: errors as ordinary enums, the std `Error` trait (`Show`
supertrait), `From`-based `?` conversion, and **every decoder that guards a
trust decision returns `Result`** (json/toml decode used by pm, coven, TUF,
webauthn, grants). Malformed input is an error, never a default.

The deferral logic inverts pre-1.0: break-don't-deprecate means the one-cut
migration is cheapest *now*, when there are zero external users to break.
Deferring "behind demand" to post-0.1 guarantees the cut lands on real users.

### D4 — Concurrency: `chan` is the public center

Per the accepted Go/CSP design, `chan` (spawn, channels, select, for-await)
is what the book teaches and what users reach for first. `task`/`future`
are demoted to layered internals with one paragraph stating their
relationship; the duplicated combinators and executor copy are deleted
(BUG-254). Generators/`iter` own lazy sequences; they are not a second
concurrency story.

RFC-0059 Increment-2 Step 2 (scalar executor synthesis) is deferred beyond 0.1.
The heap-payload OOM ceiling (~10k messages) is an **accepted, documented 0.1
limitation** — stated in the book's concurrency chapter, not discovered by
users. RFC-0036's recursive `$rdrop` / move-borrow oracle also stays deferred:
it is UAF-risk compiler-central work ("a wrong dec is a use-after-free"), the
wrong risk profile for a release push.

### D5 — Delete the second language

The kill list. Each item is a deletion, before 0.1, no compat aliases:

- **The linker's bare-name fallback.** It resolves calls against *every*
  function including non-`pub` ones, making module privacy decorative
  (BUG-451, 452). Deleting it is what makes `pub` real and closes most of
  the RFC-0042 residue (229, 216, 287) at the mechanism level.
- **The source-spellable compiler namespace.** The lexer reserves
  `__`-prefixed identifiers and `Trait__Type` manglings outside generated
  code. One lexer rule closes a soundness class (BUG-441/442/443) and ends
  `__erase`-as-a-public-cast (BUG-459 — an arbitrary unchecked cast, HIGH).
- **`cmp.member` / `index_of` / `count` / `unique`** — byte-identical to
  `list.*` post-RFC-0046. Gone.
- **The second sort** (BUG-253), **chan/task combinator copies** (BUG-254),
  **duplicated coven protocol builders** (BUG-225).
- **`retain` / `without`** — already slated; the removal actually happens.
- **`__render` as any public customization path**; `to_string`-era rendering
  in docs. RFC-0053 owns rendering: the compiled-backend interpolation gap
  (BUG-529, 305) is fixed *through* `Show`, per 0067's invalidation table.

### D6 — Check before lowering

Pipeline invariant: **no user-written code is desugared before it is fully
checked, and comptime-emitted code re-enters the front of the pipeline.**
This is one structural decision that retires the whole
lowering-erases-checks class: gen/async region-safety erasure (BUG-428, 429),
the RFC-0064 var-shape check skipping impl methods (BUG-436), comptime output
missing lowerings entirely (BUG-434), and the `lines:[0]` diagnostic class
(BUG-312, 327). The RFC-0064 trio (209/213/242) lands as part of this — the
checks finally have a place to live that nothing bypasses.

### D7 — Identity fails closed

RFC-0066 is accepted and implemented for 0.1: promote (and yank /
trusted-publish) verify a real, operation-bound WebAuthn assertion via the
existing `std/webauthn` verifier — the 2FA gate stops being a string compare.
Plus the two invariants the bug ledger shows are violated today: **authority
is persisted only after authorization succeeds** (BUG-281, 419, 423 class),
and **every verification binds the signed record to the requested
coordinate** (BUG-266, 277 class; trust anchors never repinned on failure,
BUG-371). Days-scale, worst-live-risk-first.

### D8 — `fmt` round-trip fidelity is an enabler, sequenced early

`fmt` currently prints the *desugared* AST and eats trailing/inline comments
(BUG-330–334). This looks like polish; it is infrastructure: fmt is the
declared vehicle for one-cut migrations, and D3 + D5 are one-cut migrations
across std and every project. A formatter that destroys comments and emits
non-canonical (sometimes non-parseable) output disqualifies itself as that
vehicle. Fix first: re-sugar to surface syntax, preserve all comments,
round-trip property test (`parse(fmt(src))` ≡ `parse(src)` including
comments).

### D9 — Complete the protocol matrix for shipped std types

RFC-0067 model 3, made checkable: every first-class std value
(`Bytes`, `Duration`, `Set`, `Result`, `Ordering`, tuples) has deliberate
`Show` / `Reflect` / `PartialEq` behavior **or a documented reason it does
not** (BUG-530–545 cluster). Container equality routes through `PartialEq`
so generic bounds hold (BUG-478's derive-marks-custom-eq-structural is fixed
here). RFC-0069 closes the stringly `TypeInfo` representation before 0.1;
higher-order metadata beyond declaration kind and type expressions remains
future work. A matrix test enumerates type × protocol and fails on silent holes.

### D10 — One `var` convention before the first release

RFC-0087 lands as part of the 0.1 coherence cut. The current return-shape table
makes one `var` declaration mean procedure write-back, statement-only receiver
write-back, or rejection according to return type and call context. Shipping that
rule in 0.1 and replacing it immediately afterward would spend the project's
cheapest breaking-change window on a known temporary model.

Before the tag, `var` means synchronous move-in/move-out for every resolved call,
independent of return type and expression position. The cut includes the
type-resolved migration census, nested-place parity gate, convention-bearing
function values, auxiliary-result statement ergonomics, and RFC-0051 performance
non-regression required by RFC-0087. No asynchronous `var` parameter, lifetime
surface, or no-copy extraction claim enters the blocking set.

Implementation status (2026-07-16): shipped. RFC-0087's current-truth ledger
records direct interpreter/compiled conformance, exact source diagnostics, the
compiler-resolved 439-entry corpus census, and the seven-kernel RFC-0051
optimized-versus-forced-copy gate. RFC-0088's semantic amendments are folded
into RFC-0087, so D10 has one normative source contract.

## Out — the deferral ledger (0067's "accepted residual" bucket)

| Item | Status decision | Where it's documented |
| --- | --- | --- |
| RFC-0005 residual reference boundaries | closed generic aggregates, `Option`/`Result`, typed closure environments, and reference-bearing lists implemented; `Dict`, open generic ABIs, region copy-out, and isolated-worker crossings remain reject-first | SECURITY + book caps chapter |
| RFC-0036 $rdrop / move-borrow oracle | deferred | RFC tracking note |
| RFC-0059 Increment-2 Step 2 | deferred beyond 0.1; ceiling documented | book concurrency chapter |
| RFC-0031 SIMD | deferred with explicit revival conditions | RFC tracking note |
| RFC-0062 closure elision | **implemented**; default-on after one full-matrix soak | RFC status flip |
| LSP depth (reuse the `check` pipeline) | deferred; only mode-opt enforcement holes fixed | bugs stay open below LOW gate |
| coven-namespaces / execution-plan / implementation-roadmap staleness | truth pass: reconcile the 0011 contradiction, close or refresh all three | part of the 0061 RFC-drain |

No fifth states: everything open is in a decision above or in this table.

## Sequencing

1. **Start D1 immediately in the main loop** (longest pole; runs in parallel
   with everything below, which is agent-shaped).
2. **D7 identity fixes** (days, worst live risk, independent) and **D8 fmt
   fidelity** (enabler) first among the agent work.
3. **D2** one-checker mechanism + acceptance-differential gate.
4. **D5** kill list (one-cut, mechanical parts via the now-trustworthy fmt).
5. **D3** error cut, then **D6** pipeline reorder, then **D10** uniform `var`,
   then **D9** protocol matrix.
6. Docs truth pass (0063 §1/§6), 0061 operational checklist, tag 0.1.0.

## What this buys

After this set: every README claim is enforced in the artifact users run;
every concept has exactly one spelling because the duplicates no longer
exist; the checker's acceptance set *is* the parity contract and a fuzzer
arm keeps it that way; trust boundaries are fail-closed by type (`Result`)
rather than by discipline; and every deferral is a row in a ledger, not a
surprise. That is RFC-0063's "proud release" made concrete — and it is the
difference between draining 273 bugs and fixing seven diseases.

## Non-goals

- No new features. Every decision here deletes, unifies, or enforces.
- This does not reopen decided designs (Go/CSP, break-don't-deprecate,
  RFC-0051's retained in-place family). It applies them.
- Post-0.1 direction (higher-order TypeInfo metadata, LSP-on-check, stage-4 GC, SIMD
  revival conditions) is named but not designed here.
