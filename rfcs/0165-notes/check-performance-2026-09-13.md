# Canonical `witchy check` performance, 2026-09-13

Prompted by the gate cost profile: the nine costliest tests (half of all suite
CPU) each compile a task/channel program through the canonical route, and
`witchy check examples/channels/src/channels.witchy` alone took about 29 s of
CPU on a quiet machine. Type checking and monomorphization are 0.6 s of that;
the rest is the canonical pipeline. Inclusive profile (`sample` on the debug
binary, cooperative_map, before this change), as a share of compile time:

| stage | share | mechanism |
|---|---|---|
| `check_initial` (memory checker, first pass) | ~25% | dataflow over blocks; `meet` and `promoted_object` dominate |
| `validate_final` → `recheck_final` (full second checker pass) | ~30% | identical module re-checked after its digest matched the initial witness |
| `validate_candidate_abi_bindings` | ~22% | decoded the whole layout bundle once per signature comparison (`candidate_signature_matches_source_with_records`) |
| `authorize_wasm_ir` → `owner_bridges::build` | ~15% | `OwnerBridgeRecipe::id` (SHA-256) recomputed inside an O(n²) duplicate scan |
| encoder, lowering, everything else | ~8% | |

## Landed here (no change to what is checked)

- One decode of the layout bundle per binding pass, threaded into the
  signature comparison (`candidate_signature_matches_source_with_layouts`);
  `LayoutBundle::decode_canonical` additionally memoizes its validated result
  by the content digest of the bytes within the process (same bytes, same
  process; nothing is trusted that was not decoded and validated here).
- Owner-bridge recipe ids hashed once per operation and deduplicated through a
  map instead of a linear rescan.
- Checker lookups: functions indexed by signature (`FunctionIndex`) so an
  indirect call site finds its targets without scanning every function on
  every pass; the nullable-slot admission scan is one pass over the catalog;
  `DropRef` uses the object's layout id instead of re-hashing the descriptor;
  `promoted_object` skips the whole-state scratch clone when the object has no
  loaded length to promote from.

Measured on a loaded machine (three alternating runs, minimum user seconds):

| example | before | after |
|---|---|---|
| channels | 28.0 | 24.7 |
| cooperative_map | 34.5 | 25.7 |

## Not landed, recorded for the owners

1. **The second full checker pass.** `validate_final_inner` verifies that the
   module's digest equals the digest the initial witness recorded, then runs
   `recheck_final`, a complete `check_candidate` over the same module. That is
   the single largest remaining cost (about a third of compile time). Whether
   the recheck is a required independent re-validation or a redundancy is an
   RFC-0165 design decision, not a performance one; if it stays, it should be
   named in the memory-mode boundary as deliberate.
2. **Checker state cloning.** `transfer_edge` and `meet` clone whole
   `MemoryState` maps per edge, and `promoted_object` still clones the state
   for objects that do have a loaded length. Persistent maps or copy-on-write
   would remove most of the remaining `check_memory_cfg` time. Deferred
   because `check/memory.rs` is the most contended file in the tree right now
   (six worktrees with uncommitted edits at the time of writing).
3. `canonical_bytes()` re-encodes the layout bundle on every call; the few
   remaining decode sites pay an encode plus a digest per call.
