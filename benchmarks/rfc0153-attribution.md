# RFC-0153 Attribution Refresh

Captured on the prepared RFC-0153 baseline (`77c4da309`) with
`benchmarks/rfc0153_attribution.sh`, the release binary, and `WITCHY_OPT=all`.
The generated artifact is intentionally untracked; this note records the
selection decision that the artifact supports.

| Workload | Cursorized accesses | Retained indexed checks | Coalesced guards | RFC-0153 ownership decision |
|---|---:|---:|---:|---|
| `collatz` | 0 | 0 | 0 | Transfer: there is no live sequence-plan cost, and the experimental scalar range reducer is not in the production pipeline. |
| `nsieve` | 0 | 0 | 0 | Transfer: its list work is already outside the checked indexed path; investigate allocation/runtime ownership separately. |
| `fannkuch` | 319,638,933 | 2,606,501 stores | 9,864,090 | Retain: this is a material live cursor/guard consumer. |
| `list_index` | 10,000,000 | 0 | 0 | Retain: this is a material live cursor consumer. |

The selected deterministic sequence fields were byte-identical across all
three repetitions. `heap_bytes` and RC allocation byte totals are retained in
the raw payload but are not part of the deterministic comparison: recompiling
the same program can change allocator metadata by a few bytes. This keeps the
counter claim precise instead of treating compiler-process allocation details
as a source-program invariant.

`samply` is available for wall-time control samples, but its current capture
contains unsymbolized Wasm frames and compilation work; it cannot establish a
function-level ownership percentage by itself. The deterministic counters above
therefore select the sequence closure, while `collatz` and `nsieve` remain
transferred until a named backend/runtime owner accepts them. Promotion still
requires a matched two-warmup, twelve-pair kernel run and a usable profile for
every retained mechanism.
