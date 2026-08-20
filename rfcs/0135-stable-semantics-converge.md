---
rfc: 0135
title: "Stable observable semantics converge before support"
status: accepted
created: 2026-08-20
related:
  - "0031 (SIMD stays scalar in the interpreter)"
  - "0037 (correctness harness)"
  - "0058 (differential harness integrity; unchanged)"
  - "0115 (agent contributor workflow)"
  - "0122 (Wasm-first carrier iteration as the first instance)"
  - "oracle-only-migration.md (interpreter is not a user run path)"
tracking: "Accepted 2026-08-20. Implementation: rewrite the agent briefing, playbook, spec parity section, and CONTRIBUTING.md to this invariant. RFC-0058 harness rules stay."
---

# RFC-0135: Stable observable semantics converge before support

The briefing still tells agents that every feature must land on both backends
in the same change. That sentence is the wrong inner-loop rule. The playbook,
RFC-0122, RFC-0031, and `spec/test-footprint.md` already contradict it. This
RFC makes the contradiction a decision.

## Summary

Witchy keeps the interpreter. It stops treating the interpreter as a co-equal
implementation of every evolving runtime representation.

The invariant becomes: **stable observable semantics converge before
support.** During experimental runtime work, compiled Wasm may lead. Pure
optimizations never enter the interpreter. Disagreement is a loud error, never
a different answer. Independent expected results adjudicate correctness;
backend agreement only proves the backends match.

This RFC does not weaken [RFC-0058](0058-differential-harness-integrity.md).
A green parity gate must still be able to fail. It changes *when* a feature
must exist on both backends, not whether an empty or vacuous comparison counts
as proof.

## Motivation

"Implement it twice, in the same PR" was a good rule when both backends were
being built and every new construct was source-level. It is a bad rule now.

The compiled backend is the only user run path
([oracle-only-migration](oracle-only-migration.md)). The interpreter is the
parity oracle, the `comptime` evaluator, the in-language test runner, and the
effectful build-step executor. Those jobs need a source-level evaluator. They
do not need a second copy of Wasm GC encodings, packed layouts, SIMD, or the
week's experimental reference carrier.

The 2x cost is not measured, and it is not uniform:

- Syntax and type-system work is shared. Parity adds little.
- Runtime carriers, references, async frames, `Dynamic`, and host operations
  can cost 2x or more because representation and debugging are duplicated.
- Pure Wasm optimizations should require no interpreter work.

Parity still buys things that matter:

- An independent check on Wasm codegen, ABI, memory, and capability behavior.
- Executable source semantics for `comptime`, Witchy tests, and build steps.
- Protection against silent compiled-backend corruption.

It has a ceiling. Both paths share parsing, linking, checking, and some host
machinery, so they can agree on the same bug. Independent expected-value
tests are the more authoritative oracle. `spec/test-footprint.md` already
says this; `tests/misc/semantic_conformance.rs` already exists to prove it.
The agent briefing does not.

The failure mode we are in: an agent reads `witchy-dev` section 0, then
duplicates a moving ABI in the interpreter to keep `witchy parity` green on
an intermediate slice. That work is usually discarded when the carrier
changes, and it trains the interpreter to look like Wasm.

## Design

### The invariant

> Stable observable semantics converge before support. During experimental
> runtime work, Wasm may lead. Pure optimizations never enter the interpreter.
> Disagreement is a loud error, never a different answer. Independent expected
> results adjudicate correctness; backend agreement only proves the backends
> match.

"Support" means the feature is no longer experimental: an RFC row may be
marked `PROVEN`, the surface is documented as current in `spec/`, and
interpreter-only consumers (`comptime`, `witchy test`, effectful build
steps) may observe it.

### Six buckets

| Kind of work | Interpreter | Compiled Wasm | Inner-loop check |
|---|---|---|---|
| Syntax, typeck, diagnostics | shared frontend | shared frontend | focused type and diagnostic tests |
| Stable language semantics | implement | implement | expected result plus interpreter/Wasm |
| Experimental runtime representation | named debt; loud error | implement | expected result plus Wasm; named interpreter debt |
| Pure optimization (SIMD, SROA, bounds elision, local pruning) | never | implement | expected result plus scalar Wasm plus optimized Wasm |
| Capability, confinement, ABI security | implement | implement | strict differential; no debt |
| Full parity corpus | | | stabilization, release, and periodic CI |

SIMD is the clean example, already decided in [RFC-0031](0031-simd-stdlib-hot-loops.md):

```
expected result == scalar interpreter == scalar Wasm == SIMD Wasm
```

The interpreter stays scalar forever. A SIMD slice adds only the SIMD Wasm
path and checks that optimization against the existing semantic result.

### Experimental runtime work carries a debt ledger

"Wasm-first" without a named debt becomes "never converges." Every
experimental slice that skips the interpreter must record:

1. a language-level fixture with the intended observable result;
2. a focused compiled-Wasm check for that fixture;
3. the missing interpreter boundary;
4. the milestone at which interpreter and Wasm must agree.

RFC-0122's ledger is the template. This RFC generalizes it off opt-mode
references onto any unsettled runtime representation: async frames, `Dynamic`
carriers, host ABI experiments, layout experiments that are not
output-invariant optimizations.

Interpreter work becomes required when the representation and surface
contract are stable, before the RFC row is marked `PROVEN`, before the
feature exits experimental status, or when an interpreter-only consumer
needs it. At that point the same fixture becomes a differential check.

Do not duplicate a changing backend design merely to keep parity green
during iteration.

### Loud error stays

An unimplemented interpreter path for an experimental Wasm feature must
trap, reject, or be unreachable from interpreter-only consumers. A scalar
interpreter quietly producing a different answer than the carrier is the
failure this policy exists to stop. "Parity later" does not license a
silent split.

[RFC-0045](0045-compiled-trap-diagnostics.md) still applies to supported
semantics: once both backends implement a behavior, abort diagnostics
compare byte-for-byte.

### Comptime and build are not just an oracle

The interpreter *is* `comptime`, `witchy test`, and effectful build steps.
A feature those consumers need is on a real execution path. If the feature
is required at compile time or in an in-language test, the interpreter is
on the critical path now. Wasm-first with debt is only for compiled-runtime
representation work that those consumers do not yet observe.

### Independent expected results are required at support

Declaring a feature supported requires all three:

1. an independently stated expected result or rejection;
2. interpreter agreement with that result;
3. compiled-Wasm agreement with that result.

Backend agreement alone is not enough. The conformance corpus in
`tests/misc/semantic_conformance.rs` stays the place for reviewable exact
values; `src/example_tests/` remains the broad language matrix. If dual
implementation recedes during experiment, the independent oracle has to
grow, not shrink.

### What leaves the inner loop, and what does not

The *full* parity corpus belongs at stabilization, release, and periodic
CI. It does not belong on every experimental carrier slice.

Focused `witchy parity` on a *stable* semantic change stays in the inner
loop. A one-file differential check is cheap and is the point of keeping
the oracle. Agents must not read this RFC as "skip `witchy parity` until
release."

What leaves the inner loop:

- implementing experimental carriers in the interpreter;
- running the whole example sweep on every slice.

[RFC-0058](0058-differential-harness-integrity.md) still governs the
corpus when it runs: fail-closed classification, vacuity guards, seeded
divergence controls, byte-for-byte abort comparison. This RFC does not
reopen that contract.

### Interpreter architecture

The interpreter models source-level behavior. It must not reproduce Wasm
carrier layouts, GC encodings, SIMD, or allocation strategies. Those are
compiled-backend concerns. When a compiled representation is an
optimization of a source-level operation, the interpreter keeps the
scalar/source form and the optimized Wasm is checked against that form.

The existing `*_cap` / `self_*` zoo in lowering stays under the
[RFC-0051](0051-memory-safety-invariants.md) / RFC-0016 rule: retained, not
deleted, and it does not grow. That rule is about generalizing
reclamation, not about cloning Wasm layouts into the interpreter.

### Agent-facing documents

Once this RFC is accepted, these documents must state the new invariant
rather than "every feature lands on both backends together":

- `.claude/skills/witchy-dev/SKILL.md` section 0
- `docs/agile-agent-playbook.md` (generalize the RFC-0122 special case)
- `spec/architecture.md` ("The parity discipline")
- `spec/language.md` (opening paragraph)
- `CONTRIBUTING.md` ("The one rule: parity")
- house terminology for "the interpreter" / "parity"

`spec/` is current behavior. The briefing and playbook are how agents
learn the rule. If they disagree, agents will implement the briefing.

## Alternatives

**Keep the old prime directive.** Rejected. It already lost in the
playbook and in RFC-0122. Leaving the briefing unrevised is how we keep
paying the cost.

**Drop the interpreter.** Rejected. `comptime`, in-language tests,
effectful builds, and an independent check on Wasm codegen still need it.
The oracle-only migration already took it off the user run path; that is
the right demotion.

**Expected-value tests only, no interpreter/Wasm agreement at support.**
Rejected. Dual evaluation still catches codegen, ABI, and memory bugs
that a single expected value will miss if the expected value was written
from the same mental model as the lowering. Keep both at support.

**Parity debt without a milestone.** Rejected. That is how experimental
work becomes permanent divergence.

## Drawbacks

Agents can misread "Wasm-first" as "skip the interpreter forever." The
debt ledger and the loud-error rule are the counter. Review should treat
an experimental slice with no named debt the same way the playbook
already treats a lowering change with neither an interpreter slice nor a
debt row: rejected.

Security and capability work stays expensive. That is intended.
Disagreement there is dangerous, and this RFC does not offer debt for it.

The briefing, the spec, CONTRIBUTING, and the playbook will say the same
thing in four places. Drift among them is how we got here. The
implementation cut should update them together.

## Prior art

- [RFC-0031](0031-simd-stdlib-hot-loops.md) already keeps the interpreter
  scalar and checks SIMD Wasm against the scalar result.
- [RFC-0122](0122-uniform-borrow-relations.md) and
  `docs/agile-agent-playbook.md` already iterate Wasm-first with a named
  interpreter debt.
- [oracle-only-migration](oracle-only-migration.md) already removed the
  interpreter from the user run path.
- `spec/test-footprint.md` already says parity cannot tell you the
  backends are *right*.
- [RFC-0058](0058-differential-harness-integrity.md) is the harness
  contract this policy continues to rely on.

The missing piece was a single decision that agents read first.

## Acceptance

This RFC is accepted when merged. It is implemented when all of the
following are true on master:

1. `witchy-dev` section 0 states the invariant above, not "two backends,
   zero silent divergence" as a same-change dual-implementation rule.
2. `spec/architecture.md` and `spec/language.md` describe current
   verification as convergence-before-support, with independent expected
   results required at support.
3. `CONTRIBUTING.md` tells contributors when dual implementation is
   required, when Wasm-first debt is required, and when the interpreter
   must stay out.
4. The playbook's Wasm-first policy applies to unsettled runtime
   representations in general, with RFC-0122 as an instance.
5. RFC-0058's harness contract is untouched.
6. Capability, confinement, and ABI security still require strict
   differential coverage with no debt.

No compiler behavior changes in the implementation cut. The tests that
already enforce RFC-0058, semantic conformance, and capability
differential coverage stay.
