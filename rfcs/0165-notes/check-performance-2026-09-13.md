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

## Second batch (same day)

- Dev-profile test builds optimize `sha2` and `blake3` (admission hashes on
  every digest); release builds are unaffected.
- The oracle decodes the layout bundle once per engine instead of inside every
  projection check, static-data decode, and owner-environment construction.
- The structured encoder and its plan mirror index blocks by id; the linear
  body scan per lookup had made the join search quadratic in block count for
  scheduler-sized functions. `owner_bridges::build` indexes owner families the
  same way. A whole-run profile after this (45 s sample over one
  `cooperative_map` compile) puts the two checker passes at about 17% and 18%,
  the structured encoder at about 19%, its plan mirror at about 19% (both
  dominated by `find_join`'s reachability searches, not block lookup), and
  typeck, monomorphization, linking, and lowering at about 13% together. An
  earlier mid-run sample under-read the emitters; this paragraph supersedes it.
- The oracle example shards pin `for_await` and `worker_pool` to light shards
  so no shard carries two task examples.

Measured on a loaded machine (minimum of alternating runs, user seconds):

| example | 2026-09-12 binary | after both batches |
|---|---|---|
| channels | 27.5 | 23.8 |
| cooperative_map | 36.9 | 19.5 |
| conventions | 28.4 | 23.1 |

## Not landed, recorded for the owners

1. **The second full checker pass.** `validate_final_inner` verifies that the
   module's digest equals the digest the initial witness recorded, then runs
   `recheck_final`, a complete `check_candidate` over the same module. That is
   the single largest remaining cost (about a third of compile time). Whether
   the recheck is a required independent re-validation or a redundancy is an
   RFC-0165 design decision, not a performance one; if it stays, it should be
   named in the memory-mode boundary as deliberate.

   **Resolved 2026-09-19 (`check::cache`).** The recheck stays and still
   runs every module-level check (contracts, owners, host imports, static
   data, tail-site completeness). What changed is the per-function
   `check_function` verdict: it is a pure function of the function's
   canonical bytes plus the signatures, imports and catalog entries,
   layouts (with nominal bindings and closure entries), and tail
   obligations it names, so an admitted verdict is memoized per process
   under a BLAKE3 key over exactly those inputs and replayed when they
   recur. The final recheck therefore replays every function whose inputs
   are byte-identical to the initial pass and rechecks any that are not;
   rejections are never memoized. The same key lets a lowered standard
   library function (the async executor's 11,039-block scheduler is the
   expensive case) be checked once per process rather than once per
   program. `WITCHY_SAFEWIR_CHECK_CACHE=off` restores the full second pass.
   Verdicts that cost at least 10 ms are additionally persisted across
   processes when `WITCHY_SAFEWIR_CHECK_CACHE_DIR` is set (opt-in; the
   repo's cargo config sets it for tests, which are de facto trusted),
   under a directory named by a digest of witchy-wir's sources.
2. **Checker state cloning.** `transfer_edge` and `meet` clone whole
   `MemoryState` maps per edge, and `promoted_object` still clones the state
   for objects that do have a loaded length. Persistent maps or copy-on-write
   would remove most of the remaining `check_memory_cfg` time. Deferred
   because `check/memory.rs` is the most contended file in the tree right now
   (six worktrees with uncommitted edits at the time of writing).
3. **The structured emitters' join search.** `find_join` runs several graph
   searches per branch (`distances_until`, `reaches`, `reachable_avoiding`,
   `only_terminates`, and `all_paths_reach_or_terminate` per merge candidate,
   memoized by a sorted, cloned merge set) in both the encoder and the plan
   mirror, about 37% of a task-program compile between them. Precomputing
   post-dominators once per function would replace most of it, but the
   encoder and the mirror must keep choosing identical joins, so that is a
   change to make in both at once with the parity corpus as the adjudicator.
   The plan mirror itself is a full second emission whose only purpose is to
   compare against the encoder's output; sharing the control-flow decisions
   and re-deriving only the instruction tokens is a design question of the
   same kind as the second checker pass.
4. `canonical_bytes()` re-encodes the layout bundle on every call; the few
   remaining decode sites pay an encode plus a digest per call.
5. The parity command's oracle leg re-lowers and re-validates the module
   (`validate_checked_module_for_oracle`) rather than reusing the compile's
   validated graph. That independence is deliberate (RFC-0135); it is a
   third checker pass per three-leg test, worth naming when weighing the
   second.
