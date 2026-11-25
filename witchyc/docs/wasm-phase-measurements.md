# Wasm phase observations

`wasm-measure` retains schema `witchy.scalar.measure.v1` and its existing flat
nanosecond fields. The added `phases` object contains exclusive inner intervals:
`static_check` splits primary typed checking, VC generation, and concrete
planning; `fresh_proof_check` splits the successful proof path described below.
These observations are outside the deterministic checked receipt, generated
proof module, model identities and artifact binding. They are not admission
evidence or a performance improvement. Changing compiler bytes naturally changes
the existing compiler identity.

| Proof phase | Included work |
| --- | --- |
| `goal_resolution` | Source-model classification, authored ghost-goal resolution, combining source goals, and cheap inventory counts |
| `module_build` | Source/model/audit/statement/request/module byte hashes, Lean module construction, and proof inventory construction |
| `compiler_hash_a` | Resolving the current executable path and the complete first compiler-file hash |
| `configuration` | Resolving and validating the operator Lean root and executable |
| `installation_hash_a` | The complete first trusted installation snapshot/hash |
| `scratch_setup_write` | Private scratch directory, generated source write, and output-file creation |
| `child_spawn_to_exit` | Command construction/spawn through observed exit, including polling and output-size checks |
| `output_audit_parse` | Reading outputs, status/error/unique-marker validation, and dependency parsing; excludes the intervening second installation snapshot |
| `installation_hash_b` | The complete second trusted installation snapshot/hash |
| `compiler_hash_b` | The complete second compiler-file hash |

`child_spawn_to_exit` includes Lean startup, import loading, elaboration, tactics,
kernel checking, audit execution and scheduling/polling effects. It must not be
called kernel time. The existing ten-second deadline still starts after spawn;
the existing single invocation, trust setting, environment, output limit,
kill/wait behavior, and closed audit are unchanged. Installation and compiler
hashes remain complete before/after checks, with no reuse or shortcut.

The proof object also has a `counts` object: selected goals, generated module
UTF-8 bytes, total expected-statement UTF-8 bytes, sum of already-recorded checked
nodes, compiler hashes, and installation snapshots. These do not count digest
file reads or bytes; no digest instrumentation is added. `checked_nodes` is the
existing VC/ghost accounting, not Lean kernel node count.

Inner intervals are disjoint. Existing flat `static_check` and
`fresh_proof_check` are enclosing totals, not additional additive phases. Sum
inner intervals only within their corresponding total; subtracting that sum
leaves untimed bookkeeping, clock/assignment overhead, final receipt-string
construction, destruction and scratch cleanup. Do not add an outer total to its
children or interpret a residual as an identified hotspot. The output-validation
phase accumulates two disjoint intervals around the second installation hash.

The zero-goal path reports zero child time, hash/snapshot counts, module bytes,
and other unused proof phases. It resolves no Lean environment. Goal resolution
still takes time. Only successful compiler operations return the measurements;
failed proof/type checks preserve the existing diagnostic API and do not return
a partial timing report. Rejection latency needs an external whole-process
measurement and must remain separate from successful edits.

Use optimized pinned binaries, immutable source fixtures, guarded host
reservations and matched interleaved repeated samples before attributing a cost.
Compare measurement overhead with an uninstrumented exact-base build. Preserve
raw results outside tracked source. No baseline, speedup, cache behavior, I/O
volume or dominant phase has been established by this instrumentation patch.
