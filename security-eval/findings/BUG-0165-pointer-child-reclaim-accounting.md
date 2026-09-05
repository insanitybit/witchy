# BUG-0165: pointer-child reclaim and destructor accounting

Status: fixed on `impl/rfc0165-interpreter`

The canonical SafeWIR checker admitted two ownership-loss routes for
pointer-bearing collections:

1. `Reclaim` could free a parent allocation without recursively releasing its
   owned child slots. Pointer-bearing parents must now use `DropRef`, whose
   lowering performs recursive release.
2. A dynamic initialized-prefix fact could hide a separately stored owned child
   outside the collection's logical length. Destruction follows logical length,
   so that child would become unreachable without release. The checker now
   rejects any leftover owned slot when accepting a consolidated prefix.

The recursive `DropRef` backend also reserved four scratch locals per frame
while using five. Both the encoder and independent admission-plan mapping now
use five-local frames. The old behavior failed closed during encoding or Wasm
validation rather than emitting an unsafe artifact.

Focused regressions:

- `pointer_bearing_reclaim_requires_recursive_drop`
- `dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot`
- the 43-test `canonical_pipeline::tests::` compact corpus
