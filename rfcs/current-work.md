# Current work and parked decisions

Checkpoint: 2026-10-03, master `c55ce5c41`. This is a work-selection map,
not a claim that every retained contract is implemented. Current source,
focused tests, and a terminal merge-queue gate determine executable behavior.
The full historical plans and evidence remain in Git at this checkpoint:
`git show c55ce5c41:rfcs/0153-acceptance-ledger.md`.

## 1. Normal-mode language and compiler workflow

[RFC-0167](0167-witchy-language-contract.md) defines normal-mode source
semantics. Its bounded compiler-tooling milestone is complete, with gates and
limits in the [acceptance ledger](0167-acceptance-ledger.md). Preserve the
checked ownership, `var` write-back, immutable views, `must` disposition,
typed comptime, and installed check/fmt/compile/run behavior under admitted
Wasm. Historical partial rows are not a mandate to implement every carrier.
Accept a later extension only for one concrete user-visible program with a
reusable ownership proof across the checker, lowerer, and SafeWIR, plus
admitted Wasm evidence. Historical source-interpreter parity is not a gate.
`mode opt`, cross-mode calls and opt-defined types remain deferred under
[RFC-0173](0173-opt-mode-contract.md).

## 2. Capabilities and host grants

[RFC-0167](0167-witchy-language-contract.md) owns the source meaning of typed
capabilities, effectful calls, delegation, and purity. [RFC-0126](0126-capability-effects-contract.md)
owns authenticated roots, rights narrowing, footprint and grant binding, and
host confinement. Keep the normal-mode capability workflow executable end to
end; extend effectful standard-library coverage only with host-boundary evidence.

## 3. SafeWIR safety floor; stronger work deferred

[RFC-0165](0165-safewir-canonical-ir.md) owns the canonical SafeWIR route, and
the [schema-1 judgment](0162-safewir-judgment.md) defines a narrower independent
memory-validation contract. The [RFC-0165 ledger](0165-acceptance-ledger.md)
records bounded evidence and historical checkpoints. It does not certify a
compiler-wide memory-safety theorem. [RFC-0163](0163-authority-validated-codegen.md)
remains a proposal for stronger authority and final-code validation; its
historical ledger cannot substitute for a current implementation audit.

Retain existing production checks and artifact admission. Defer stronger proof
claims and new optimization passes until the language and capability contracts
settle. Do not bypass a check or treat bounded evidence as compiler-wide memory
safety. Any later new optimization must have (1) a precise allowed rewrite and independent
behavior-preservation check covering values, effects, and traps, (2) final
SafeWIR memory/ownership validation, (3) authenticated host-authority and
artifact admission, and (4) focused adversarial and matched-workload evidence.
A final safety check alone cannot prove that an optimization returns the same
value. The current same-block bounds-guard pass has an exact rewrite relation
and final validation; any stronger pass needs its own relation.

## 4. Runtime performance: measure first

[RFC-0174](0174-incremental-compiler-cache.md) owns the proposed incremental
compiler cache. It is a mechanism proposal, not evidence that compilation is
already incremental. Cache keys, invalidation, authority replay, and matched
cold/warm/cross-process measurements must be accepted before implementation
claims are made.

Older RFC-0143, RFC-0146, and RFC-0153 benchmark ratios are revision-specific.
Use the current binary, equivalent Witchy and Go results, paired kernel clocks,
and end-to-end wall clocks before selecting work. Do not promote a change from
one benchmark or a source-mode switch. Retain deoptimization, trap, ownership,
and SafeWIR checks as promotion gates.

- Dictionary work: RFC-0143's Swiss control-group and SIMD mechanisms landed,
  but its proposed flat-bucket representation was not accepted as written.
  Reopen representation work only when current profiles show a general gap.
- Sequence bounds: RFC-0153 tracks 0-5 and many 6C slices landed. Its old 6D
  adversarial/generalization row and 6E promotion claim were incomplete at the
  last ledger checkpoint. Recheck current-tip facts, source behavior, and
  matched workloads before closing them; a Go-parity target is a measurement
  goal, not permission to elide an unproved guard.
- Calls and async: RFC-0146's select fusion, sequence plans, and packed-Bool
  kernels landed. Its direct-call inlining trial missed its whole-workload
  threshold; recursive inlining lacked attributable profiling, and borrowed
  hashing did not meet its entry criterion. Reopen only with current evidence.
- Scalar loops and compilation/startup: profile the present code before
  choosing a generic scalar rewrite, standard-library precompilation,
  interning, or a protected compiler service. The old RFC-0164, RFC-0169,
  and RFC-0172 documents were unaccepted proposals, not simultaneous plans.

## 5. Other product proposals

The hosted Coven, Glamour, native-extension, dynamic-code, and expanded effect
system proposals are parked. Revive one with a new scoped decision when its
product boundary is actually scheduled. Preserve capability and confinement
checks for the current compiler and runtime under
[RFC-0126](0126-capability-effects-contract.md); deleting a parked roadmap
never relaxes those checks. RFC-0168's toolchain split and RFC-0166's symbolic
grants likewise require an explicit current dependency and acceptance gate.

## Retirement rule

Removed RFC bodies, companion ledgers, notes, and reports are historical inputs
recoverable from Git. The [retirement index](retired-rfcs.md) gives the recovery
command and authority map. Machine-readable fixtures consumed by builds or
checks remain in the repository. A new RFC must change a retained contract or
specify a scheduled, independently verifiable mechanism; otherwise update this
work map or an implementation test.
