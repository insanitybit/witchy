# Unique pop write-back lost at loop joins

The queued owned-pop candidate rejected the existing
`rfc0087_uniform_var::list_pop_projected_coordinates_and_nested_payloads_on_three_legs`
test with `InvalidOwnership { function: FunctionId(2), object: ValueId(1512) }`.
This was an admission coverage regression, not evidence of an admitted unsafe
execution. The following scalar loop independently reproduced it:

```witchy
fn main(console: Console):
    var total = 0
    var numbers = [1, 2, 3]
    for ignored in [0, 1, 2, 3]:
        total = total + (numbers.pop() ?? 0)
    console.print("${total} ${numbers.length()}")
```

The expected output is `6 0`. Separate projected-place and nested-payload
probes already agreed across source and admitted Wasm.

## Cause and correction

The late `demote_shared_reference_phis` pass classified every direct and
indirect call destination as Shared, including authenticated Unique
write-back results. Earlier call preparation correctly used those result
contracts, so the late demotion invalidated the loop receiver's authority.
The correction classifies each result independently. Actual clones and
shared results still demote their incoming phi lanes.

Removing that demotion exposed `IncompatibleMemoryJoin`: a fresh scalar
list and its Unique call write-back had identical identity, ownership,
layout, count, and capacity, but different completeness-summary bits.
`transfer_edge` previously established proven predecessor completeness only
for Shared phis. It now invokes the same existing completeness judgment
for Unique phis too, before renaming. It does not erase slot/range facts,
infer completeness from capacity, or weaken sole-token checks. Missing
pointer-prefix evidence still cannot acquire a completeness summary.

The retained-snapshot while-loop regression then exposed a stale lowering
cache: the checker legitimately demoted an uncarried snapshot's extent at
the loop join, but lowering reused pre-loop count/capacity ids without
reloading its headers. The while exit now invalidates those outer-binding
extent caches; explicitly transported exit lanes retain their metadata.
Subsequent snapshot accesses re-establish extent and bounds from real header
loads, without changing the saved value or weakening admission.

## Focused evidence

The original failure and the second join refusal were both executed.
The corrected native core/lowerer batch passed 19/19 tests, including the
isolated loop, direct/indirect result classification, real-sharing demotion,
generated-helper no-copy checks, TakeLast controls, and predecessor-evidence
negatives. After the snapshot-cache correction, the full existing
RFC-0087 module plus pop filters passed 28/28 tests, including the original
gate regression and the new scalar/reference loop and snapshot coverage.
This is focused evidence, not a full gate or landing claim.
