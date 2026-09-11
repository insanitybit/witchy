# Task ownership integration: false refusals and remaining execution failures

Status: candidate fixes, not landed. These results do not establish compiler-wide
memory safety or optimizer correctness.

## Repaired integration defects

- Indirect `var` calls used body-derived result contracts even though the unified
  ABI returns owned references. Ref writeback results now declare Shared without
  inventing alias or reallocation provenance. The indirect-call summary retains
  its non-destructive effect ceiling and rejects provenance-bearing alternatives.
- Match and conditional writeback planning could take a length definition from a
  different predecessor's fact snapshot. Each predecessor now supplies its own
  header facts and outgoing extent arguments. Count and capacity remain distinct.
- An empty initialized range could suppress reconstruction of the independently
  established current owned prefix. Empty ranges cannot authorize element loads;
  prefix reconstruction still requires complete owned-slot initialization and
  the exact current count. Unproved count growth remains rejected.
- Nested loop sharing could leave a Ref block parameter annotated Unique after
  its incoming values became Shared. Lowering normalizes those annotations before
  release placement; the independent ownership checker remains authoritative.

Focused tests already passed on the candidate:

```sh
export CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-reflect-specialization-target
RUSTC_WRAPPER= CC=cc cargo test -p witchy-wir --features native --lib indirect_writeback_summary_requires_owned_results_without_provenance_claims
RUSTC_WRAPPER= CC=cc cargo test -p witchy-wir --features native --lib empty_range_cannot_replace_current_owned_prefix_evidence
RUSTC_WRAPPER= CC=cc cargo test -p witchy-lower --lib indirect_var_list_replacements_preserve_caller_aliases
RUSTC_WRAPPER= CC=cc cargo test -p witchy-lower --lib branch_local_list_headers_do_not_escape_to_other_writeback_edges
```

The rebuilt production CLI also executes `hello`, `wrap`, and `jq` with their
expected output after the loop-annotation repair. This is execution evidence,
not a new corpus census.

## Failures observed during integration

The unchanged channels example passes SafeWIR validation but fails encoding:

```text
WasmValidation("function body size count exceeds limit of 7654321 (at offset 0x1df5a6)")
```

A process sample locates compilation cost in structured CFG emission and its
`all_paths_reach_or_terminate` traversal. Memoizing negative as well as positive
path judgments fixes repeated traversal; the focused diamond and cyclic-suffix
tests pass. A separate code-size cause remains: each owner drop inlines a cleanup
dispatcher across all owner families. That dispatcher needs one generated
backend helper. Increasing Wasm limits is not a fix.

The smaller `Action = Empty | Emit(fn() -> Nil)` fixture stores captured
Console/String closures in a list, replaces one variant while retaining the
old list, then executes both lists. Aggregate host registration previously
incorrectly demanded a callable table identity from a non-callable aggregate
family. Separating the aggregate and closure registration paths exposed a second
bug: recursive cleanup emitted an owner-type ID where the guest dispatcher
expects the authenticated family signature. Runtime owner bindings now retain
that distinct cleanup identity. The tracked three-legged regression passes:
`rfc0165_messages::canonical_opaque_variant_list_replacement_preserves_old_closure_three_legs`.
Its output is `one`, `empty`, `two`, with no live heap objects in the canonical
oracle or admitted Wasm. This closes the small aggregate execution failure, not
the still-pending full channels program.

## Correspondence follow-up

Local/transitive closure capture binding checks do not yet prove full source
SSA-value correspondence across shadowed bindings. Existing type, liveness,
family, and affine checks remain necessary but do not prove that lowering chose
the source-intended value. This is a source-semantic correspondence obligation;
no capability forgery or false memory authority has been demonstrated here.

## Current result

Candidate `c7e9fca50` passes the four-test message shard, including the unchanged
channels program on source, oracle, and admitted Wasm (160.49 seconds; zero
leaked objects). After the encoder repairs, execution exposed
`owner list transfer retired a retained element`: cleanup from an earlier
replacement was being confused with retirement of a current element. Mutation
now returns cleanup through the private bridge ABI and the existing backend
helper drains it immediately. Current-element retirement checks remain intact;
retired payloads do not accumulate until final list destruction. Native accounting
tests cover duplicate tokens, shared aliases, and immediate replacement cleanup.
Landing and the full-project acceptance obligations remain pending.
