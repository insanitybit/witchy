# Shared extent meet lost a common logical count

The nullable-capability integration gate rejected the existing jq three-legged
probe with `InvalidOwnership`, object `ValueId(447)`. CLI reproduction selected
`json.unescape`; function numbers differ between harnesses.

`MemoryChecker::meet` demoted differing counted capacities to `HeaderPrefix`
before applying the existing shared-object meet. Both predecessors retained the
same authenticated logical count, but the fallback erased it. A later shared
phi consequently lacked a transported extent and failed closed.

Apply the existing shared meet before generic extent demotion. Its identical
identity/layout/count/header/stride, Shared ownership, and managed-storage
requirements are unchanged. Differing capacities meet conservatively at count;
no write authority is granted.

Focused evidence on the repair candidate:

- `shared_extent_meet_preserves_count_before_header_demotion`: passed.
- `shared_phi_capacity_guard_cannot_authorize_read_past_length`: passed.
- `same_identity_ownership_meet_rejects_identity_consumption_and_alias_changes`: passed.
- CLI `witchy parity` for hello, wrap, jq: agreement on 3, 5, 7 output lines.
- `cargo test -p witchy --test misc rfc0165_recursive_generic::jq_recursive_json_queries_execute_across_three_legs -- --exact`: one passed.
- Independent bounded review found no concrete safety defect. The unit test
  isolates the meet using snapshots; the jq probe exercises authenticated
  predecessor construction and all three execution legs.

Builds use `CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target`.
This is coverage/regression evidence, not compiler-wide memory-safety proof.
