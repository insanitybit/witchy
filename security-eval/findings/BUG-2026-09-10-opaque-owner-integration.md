# Canonical opaque closure integration failures

Status: repaired on candidate `99e5f3675`; not landed.

The unchanged `examples/plugin_host/src/plugin_host.witchy` and focused mixed
String/Console closure probes exposed these concrete failures:

- Function lists cloned a reference, then required `StoreTransfer::MoveUnique`.
  `owned_for_storage` now copies the registered callable shell and retains its
  immutable environment. The checker was not weakened.
- Function-table discovery missed indirect tail calls and owner callable reads.
  The table now covers these operations; callable operands remain table-element
  ordinals, not imported-function-adjusted Wasm function indices.
- Local-type matching compared different library debug spellings for nullable
  `anyref`. Exact nullable-type normalization fixes this fail-closed mismatch.
- Per-block planning omitted owner scratch locals when the owner operation was
  in another block. Scratch presence now depends on the whole-function value
  table, retained by the independent planner.
- The oracle omitted the environment tag initialized by the constructor.
  Construct now initializes it, allowing the guarded mixed-capture read.

Four source/oracle/admitted-Wasm probes, three runtime owner probes and nine
native WIR owner probes pass. Native fixture repair preserved the owned Shared
parameter ABI, its declared release effect, negative assertions, and independent
admission. The teardown fixture now checks actual free-list reclamation:
neither root is freed on the first owner drop; final drop yields the terminated
chain `128 -> 64 -> 0`; another owner drop rejects. Dead RC headers are not
allocator liveness markers and are not required to be zeroed.

No raw fallback or new guessed authority was introduced. This evidence does not
establish compiler-wide memory safety. Queue and master-census evidence remain
outstanding; see the acceptance ledger for the remaining source-test build issue.

## Nested string result joins (pre-submit regression)

The freshly rebuilt CLI refused unchanged `examples/hello/src/hello.witchy`:
`InitialValidation(Safety(InvalidOwnership { function: FunctionId(1), object: ValueId(44) }))`.
With `WITCHY_CANONICAL_PLAN_DEBUG=1`, the first refusal reports the outer shared
phi carrying count `ValueId(45)` but retaining predecessor capacity `ValueId(28)`
outside its parameter lanes. The nested `classify` match/if reproduces it.

Both result-join lowerings recorded authenticated count/capacity parameters and
then immediately called `reload_dynamic_extent` for strings. That length-only
reload replaced the cached capacity, so the next join did not carry the original
capacity definition. Retain the result join's two explicit lanes instead; the
checker still independently validates every predecessor and all extent guards.
No validator rule, source example, or runtime bound is weakened. The focused
`nested_shared_string_joins_preserve_capacity_lane` test pins initial admission;
`hello`, `wrap`, `jq`, and `plugin_host` are the required executable regression set.
