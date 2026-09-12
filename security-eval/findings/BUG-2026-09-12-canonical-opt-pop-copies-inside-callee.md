# Canonical list.pop copies inside a promised no-copy callee

Baseline: 0301062c6. Static lowering finding, not a memory-safety bypass.

The regression source is OPT_POP_HELPER plus OPT_POP_CALLER in
tests/misc/rfc0165_reference_list_patterns.rs. A normal caller retains a
List(String) snapshot and invokes an imported opt helper with a unique var
parameter. The helper calls list.pop. Expected values are last, 1:2, last;
changing the caller to mode opt must reject the missing no-copy proof.

std/list.witchy promises O(1) extraction without copying the spine for a
unique nonempty list. The memory-mode amendment permits normal caller
preparation before entry, not payload copying inside the promised callee.

canonical_list_pop.rs::lower_list_pop_extract unconditionally invokes
copy_pop_prefix on its nonempty branch. That function allocates another list
and loops over the retained prefix, loading and storing every element. There
is no unique/no-copy branch. This remains true even if caller preparation
successfully produces private storage before entering helper.take/list.pop.
It therefore cannot discharge the callee's promise by calling this loop
normal-mode argument preparation.

The focused tests distinguish ordinary values/reclamation and opt-caller
refusal from the missing resource guarantee. They do not assert that correct
output or zero leaks establishes no-copy execution. Execution results are to
be reported separately from this static finding.

No checker, ownership rule, source promise, or production lowering was changed.
A correction needs an authenticated caller preparation boundary and an
actually non-copying unique extraction path, or a precise refusal until the
existing promise can be honored; it must not silently weaken the contract.
