# RFC-0143 current-truth and acceptance ledger

Audit date: 2026-08-23; audit base: `fa4f9844864c5ec28db3494b52499ec20c2c3749`

This ledger deliberately separates the useful Swiss-control/index work already
on master from acceptance of RFC-0143 as written. The RFC remains **proposed**.
The implementation is a dense owning dictionary plus a non-owning ordered
control/index carrier; it is not yet evidence for the specified flat-bucket,
scalar/SIMD-equivalent representation and promotion gate.

## Requirement audit

| Requirement | Status | Current evidence or exact gap |
|---|---|---|
| 64-bit hash ABI and H1/H2 split | **PARTIAL / code present** | `dict_hash` and `dict_hash_slice` return `i64`, and callers derive H1/H2. This satisfies the review correction, but no complete hash-coherence matrix is retained. |
| String/Bytes equality wording | **DOCS FIXED** | RFC states that both use the existing mode-1 byte path and `$str_eq`; no distinct Bytes equality helper is claimed. |
| Hidden word for nonempty roots | **DOCS FIXED** | RFC specifies `d-4` as the Swiss allocation/header pointer and `d` as the live count, with zero-count handling. Current helpers preserve that hidden-word convention. |
| Control encoding and mirrored group | **PARTIAL** | Master uses `0x80` empty and H2 occupied controls. The landed helpers do not implement the RFC's `0xFE` deleted/tombstone state or the specified tombstone-pressure protocol. |
| Probe sequence and SIMD group scan | **NOT ACCEPTED** | `dict_find` performs scalar one-byte control loads and linear slot increments. `dict_ctrl_h2`/mask helpers exist, but the registered vector helper is not called by the `dict_find` probe. No emitted-Wasm scalar-vs-SIMD pair proves the required paths. |
| Flat bucket storage | **NOT ACCEPTED** | The carrier stores control bytes, entry indices, and maintenance key/value lanes while the dense dictionary remains the owning/projection source. This is a hybrid index optimization, not the RFC's complete flat owning bucket layout. |
| Stable insertion order | **PARTIAL** | Focused integer WIR tests cover insert/update/projection/remove behavior, and `order_bytes_moved` exists. The full independent expected-result matrix for replace, remove/reinsert, grow, Show, reflection, and all supported key modes is absent. |
| Removal and order accounting | **PARTIAL** | Counters are exported and incremented for projection/order repair. There is no accepted churn matrix bounding retained heap growth, and the implementation does not provide the RFC's deleted-lane reuse semantics. |
| Promotion threshold definition | **DOCS FIXED** | The RFC unambiguously defines the geomean over exactly `dict_count`, `word_count`, and `knucleotide`, with the 5% geomean, 10% one-workload, 2% regression, projection, memory, churn, and portability gates. |
| Phase-0 vectorized current-index control | **NOT EVIDENCED** | The RFC requires this control row and RFC-0030 counters, but no checked-in paired control artifact or current-index vectorized implementation is present. |
| Interpreter/Wasm and scalar/SIMD differential parity | **NOT ACCEPTED** | Existing focused WIR tests prove useful integer insert/read/project/remove cases. The required independent expected outputs across scalar index, scalar Swiss, SIMD Swiss, interpreter, String, Bytes, borrowed slices, collision, and custom-`Eq` fallback rows are not retained. |
| Ownership, traps, rebuild, and memory | **NOT ACCEPTED** | Existing ownership/trap tests cover portions of the dense path. No complete unique/shared/COW/grow/rebuild/failure matrix, requested/transient/retained byte measurements, or bytes-per-live-entry comparison proves the RFC gate. |
| Portability | **NOT EVIDENCED** | No durable Wasmtime/browser SIMD artifact paired with a scalar no-SIMD artifact is recorded, and no emitted-Wasm assertion proves vector instructions only in the SIMD build. |
| Protected workload promotion | **DEFERRED** | The durable RFC-0146 Track-7 audit at `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track7-audit-d86b5ff5/track7-schema1.json` records 12-sample `dict_count`, `word_count`, and `knucleotide` baselines plus counters, but explicitly lacks RFC-0143's matched before/after representation matrix and therefore cannot promote this RFC. |

## Decision

**RFC-0143 remains proposed and deferred.** The current Swiss control metadata,
64-bit hashing, borrowed-string lookup support, and ordered projection counters
are implementation evidence and useful prerequisites, not acceptance evidence.
No performance promotion or semantic closeout is claimed. The next acceptance
slice must first retain a schema-1 artifact containing the complete size,
operation, hit-ratio, key-mode, collision, churn, ownership, memory,
interpreter/SIMD differential, and scalar/portable Wasm matrix, then apply the
RFC's three-workload promotion gate against the immediately preceding master.

This decision is independent of RFC-0146. RFC-0146's dictionary/string audit
may use the partial Swiss machinery for attribution, but it does not accept or
close RFC-0143.
