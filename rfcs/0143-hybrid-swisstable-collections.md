---
rfc: 0143
title: "SIMD SwissTable dictionary storage with stable observable iteration"
status: proposed
created: 2026-08-22
related:
  - "0016 (reference counting and reclamation floor)"
  - "0017 (codegen performance constant factors)"
  - "0026 (unique qualifier)"
  - "0028 (ergonomic mutable value semantics)"
  - "0030 (one optimization lever and differential de-optimization)"
  - "0031 (SIMD stdlib hot loops)"
  - "0033 (place-based uniqueness)"
  - "0051 (memory-safety invariants)"
  - "0122 (uniform borrow relations)"
  - "0135 (stable observable semantics converge before support)"
  - "0139 (heterogeneous dictionary lookup and entry fusion)"
  - "0140 (Wasm SIMD instruction support and control-byte probing)"
tracking: "Proposes flat compiled-Wasm Dict buckets with a SwissTable control index, an off-hot-path insertion-order vector, scalar and SIMD probe paths, and measured promotion gates. Public Dict and Set APIs remain unchanged."
---

# RFC-0143: SIMD SwissTable dictionary storage with stable observable iteration

## Summary

Witchy's compiled `Dict(k, v)` keeps its current public behavior while replacing
the scalar open-addressing index with a SwissTable-style representation for key
types that already have compiler-internal hashes. The compiled representation
stores key/value slots directly in flat hash buckets, scans 16 one-byte control
tags at a time, and keeps a separate bucket-order vector for the established
insertion-order projections.

The order vector is not consulted by lookup, update, or replacement. It exists
because `keys`, `values`, `pairs`, rendering, reflection, and stdlib transforms
return ordered values today. The interpreter remains a source-level semantic
model backed by its own ordinary collection representation; it does not
reproduce the Wasm memory layout or SIMD kernel.

This RFC changes no Witchy syntax, type, trait, capability, or standard-library
signature. It introduces no public `RawTable`, `Hash`, or `Entry` API. Unboxed
per-`(k, v)` bucket layouts, hashing for custom `Eq` types, borrowed entry
handles, and a `Set` representation change require separate evidence and
separate RFCs.

## Motivation

### Current compiled representation

The compiled backend currently stores dictionary entries densely in insertion
order as one 16-byte universal-slot pair per entry. A hidden word at `d - 4`
points to an open-addressing index whose buckets hold 32-bit dense-entry indices.
The index is at most half full. A lookup hashes a supported scalar key, probes
one 32-bit index bucket at a time, and performs full key equality for every
occupied bucket encountered.

That design has three useful properties which this RFC retains:

1. dictionary order is independent of hash placement;
2. the hash index owns no key or value roots; and
3. compound or custom-`Eq` keys have a correct linear-scan fallback.

Its indexed lookup spends four bytes per index bucket, loads and branches once
per probe position, and has no short fingerprint with which to reject a bucket
before loading its key. The current index also sits beside the dense entry
allocation, so a successful lookup loads an index bucket and then follows it to
the key/value pair.

`ToSlot` and `FromSlot` are not NaN boxing. They are the compiler's typed
conversion boundary to and from a universal `i64` slot: `i64` is already in
slot form, `i32` is extended or wrapped, and `f64` is bit-reinterpreted. This
RFC does not attribute heap allocation or dynamic tag checks to that boundary.
The hypothesis under test is narrower: compact control metadata, H2 filtering,
one hash per operation, and direct bucket storage reduce lookup work enough to
justify their representation complexity.

### Existing performance evidence

The current single-search update and borrowed-string lookup paths landed in
`1b660a51f2e508cd2f37599b2be9d2cc5891980b` and
`bcf30571e2c8d7760c809058dfc135753d43ca7e`; RFC-0139's `proposed` header is
stale tracking, not evidence that those paths are absent.

The checked-in baseline at commit
`bf84f2ac1b7fbcc2f267a680cbc633b625d8218f` records the following kernel-clock
snapshot. The harness reports the minimum of eight in-program samples, so these
numbers orient the investigation; they are not acceptance evidence for this
RFC and must be refreshed with distributions on the implementation revision.

| workload | Witchy kernel ms | Go kernel ms | Witchy / Go |
|---|---:|---:|---:|
| `dict_count` | 25.6 | 32.6 | 0.78x |
| `word_count` | 144.7 | 56.4 | 2.57x |
| `knucleotide` | 60.1 | 20.4 | 2.94x |

Go remains contextual evidence, not the promotion oracle. The decision is made
by matched before/after Witchy runs with identical source, compiler, runtime,
host, warmup, and correctness checks.

### Why order is handled off the lookup path

Insertion order is observable today. `keys`, `values`, and `pairs` return Lists;
programs may inspect their first element, zip them with other Lists, render them,
or perform effects while iterating. Making physical bucket order observable
would either allow backend-dependent answers or freeze the hash function,
capacity policy, probing algorithm, and growth history into the language.

Neither outcome is required for fast lookup. A compact vector from insertion
position to bucket index preserves the current result while leaving the lookup
kernel on flat buckets. New-key insertion appends one 32-bit bucket index;
replacement touches no order metadata. Removal already has an O(n) component in
the current ordered representation and may compact this vector. Growth rebuilds
it while rehashing.

At ideal maximum load, the proposed buckets, control bytes, and order vector are
an estimated `16 / 0.875 + 1 / 0.875 + 4 = 23.43` bytes per live entry before
headers and power-of-two rounding. The current 16-byte dense entry plus an index
sized to at least two 4-byte buckets per entry is approximately 24 bytes. This
estimate only shows that order metadata is not automatically a memory
regression; the acceptance matrix measures actual requested and retained bytes.

This is a language contract, not an instruction to preserve the interpreter's
`Vec` implementation. The Wasm layout is independent and optimized for its own
hot path.

## Design

### 1. Observable contracts

All current `Dict` behavior remains stable:

- first insertion establishes a key's iteration position;
- replacing an existing key preserves that position;
- removal deletes the position;
- inserting the removed key again appends a new final position;
- `keys`, `values`, and `pairs` traverse those positions;
- rendering, reflection, comprehensions, and stdlib transforms observe the same
  traversal order they do now;
- value semantics, copy-on-write repair, `var` write-back, and `unique` no-copy
  obligations retain their current meaning; and
- missing-key diagnostics and all evaluation order remain unchanged.

The representation is internal to compiled Wasm. It is not accessible through
reflection, `comptime`, the host ABI, or a source-level pointer API.

### 2. Representation selection

After type checking and monomorphization, lowering already classifies a
dictionary key shape.

- `Int`, `Bool`, `Duration`, `String`, and `Bytes` use the Swiss representation.
  Borrowed string queries use the same canonical byte hash as their owned
  `String` key.
- `Float` remains unavailable as a public dictionary key because it is not
  `Eq`.
- Compound keys and types with custom `Eq` retain the current dense ordered
  representation and linear equality scan. This RFC defines no structural hash
  for them and does not narrow the existing `where k: Eq` surface.

The choice is per monomorphized key shape, not per method name. All dictionary
operations for one concrete type agree on its representation.

Lowering routes each resolved intrinsic to a dense or Swiss helper family; the
hot lookup does not inspect a runtime representation tag. Internal constructor
ABIs gain the resolved key mode where allocation depends on it, including
`dict.with_capacity`. Generic Witchy functions are monomorphized before this
routing, and an unresolved key shape remains the same loud codegen error it is
today.

### 3. Swiss dictionary allocation

For a nonempty scalar-key dictionary, one reference-counted allocation contains
a fixed header followed by aligned regions. The returned `d` pointer continues
to address `count` and the RC object base remains at `d - 4`, preserving the
root convention used by generic reclamation:

```text
DictHeader
  count:          u32
  bucket_count:   u32
  growth_left:    u32
  tombstones:     u32
  ctrl_offset:    u32
  buckets_offset: u32
  order_offset:   u32
  order_capacity: u32

padding to 16 bytes
ctrl:    [u8; bucket_count + GROUP_WIDTH]
padding to 8 bytes
buckets: [(universal i64 key slot, universal i64 value slot); bucket_count]
order:   [u32; order_capacity]
```

`GROUP_WIDTH` is 16. `bucket_count` is a power of two, never less than 16, and
is chosen so the maximum live load is 7/8. `order_capacity` is at least the
maximum live count supported before the next allocation.

The control-byte encoding is the one already specified by RFC-0140:

- `0x80` — empty; encountering it terminates an unsuccessful probe;
- `0xFE` — deleted; it may be reused for insertion but does not terminate a
  probe; and
- `0x00..=0x7F` — occupied, carrying the seven-bit H2 fingerprint.

The first 16 control bytes are mirrored at `ctrl + bucket_count`. Any mutation
to a control byte in the first group updates its mirror before the operation
publishes the table. This permits an unaligned 16-byte load spanning the logical
end of the table. `ctrl` itself is 16-byte aligned; the bucket region is
8-byte aligned.

The allocation is initialized completely before a new root replaces the old
root. Checked overflow guards cover every header, padding, control, bucket, and
order-size calculation.

`dict.new()` and `dict.with_capacity(0)` retain the current eight-byte empty
root: the hidden word and count are both zero. For a nonempty Swiss root, the
hidden word at `d - 4` holds the pointer to the Swiss allocation/header, and the
count word remains the live-entry count used by the existing index-present
check. Operations check the zero count before reading the larger header. The
first insertion allocates the minimum
16-bucket Swiss representation once a transient scalar root reaches two live
entries; a one-entry root remains on the dense path so insert/remove churn does
not retain an unreclaimable carrier allocation. `dict.with_capacity(n)` for
positive `n` allocates and initializes enough buckets and order capacity for
`n` live entries without a grow, including the one-entry case. Empty
dictionaries therefore do not pay for a 16-bucket table.

### 4. Hash split and equality

The compiler-internal scalar and byte hash helpers must produce one 64-bit hash
per semantic dictionary operation for this representation. Today
`$dict_hash`/`$dict_hash_slice` return `i32`; Phase 1 widens that compiler-
internal helper ABI (and its callers) before relying on the split below:

```text
H2 = (hash >> 57) & 0x7f
H1 = hash & 0x01ff_ffff_ffff_ffff
initial_group = H1 & (bucket_count - 1)
```

The stored `String` hash and the borrowed string-slice hash consume identical
bytes, length, seed, and finalizer. A slice hit performs no owned-key allocation.
`String` and `Bytes` use the existing mode-1 byte-oriented hashing/equality
path (`$str_eq`); the representation does not invent a distinct Bytes equality
operation. Their source-level types and diagnostics remain distinct.
The hash is internal and fixed for reproducible compilation; this RFC makes no
HashDoS-resistance claim.

H2 is only a filter. Every matching control lane loads the candidate key and
runs the existing typed equality operation. Equality remains authoritative.

No public `Hash` trait is introduced. A future trait must define derivation,
custom-`Eq` coherence, heterogeneous-query coherence, seeding, and compound-key
behavior before the compiler may use it for new key shapes.

### 5. Probe sequence

Both scalar and SIMD implementations use the same group sequence. Starting at
`initial_group`, each unsuccessful step increases the stride by 16 and masks
the next position by `bucket_count - 1`:

```text
position = initial_group
stride = 0

loop:
    group = ctrl[position .. position + 16]
    examine H2 matches
    stop on an EMPTY lane when no key matched
    stride += 16
    position = (position + stride) & (bucket_count - 1)
```

For lane `i`, the physical bucket is `(position + i) &
(bucket_count - 1)`. The 7/8 load limit guarantees an empty lane and therefore
probe termination.

Lookup returns a logical `ProbeResult` containing the matching bucket, or the
first reusable deleted/empty bucket for insertion. The concrete WIR ABI may use
multiple scalar results. Upsert computes the hash once and carries this result
through lookup and insertion rather than rehashing in a second helper.

### 6. SIMD and scalar group matching

RFC-0140 owns the fixed-width WIR vector instructions. This RFC owns their use
inside the complete dictionary representation and operation protocol.

The SIMD candidate mask is:

```wat
v128.load
i8x16.splat
i8x16.eq
i8x16.bitmask
```

The empty mask uses the same sequence with `0x80`. Matching bits are traversed
with `i32.ctz`; only H2 candidates perform full equality. This is one 16-wide
group probe, not a one-cycle operation. Host instruction count and latency are
measurement results, not Wasm semantics.

Targets without fixed-width SIMD use a scalar 16-control-byte loop over the
same storage and probe sequence. Target capability selects the implementation
at compile time, so the hot loop has no runtime SIMD feature branch. Relaxed
SIMD is unnecessary for control-byte equality.

When the implementations exist, the optimization registry gains real
`dict-swiss` and `simd` consumers and increments its schema version. The
de-optimization matrix must prove:

```text
current scalar index == scalar Swiss index == SIMD Swiss index == expected result
```

No phantom optimization name lands before it selects emitted code.

### 7. Dictionary operations

#### Lookup and contains

Probe the control groups, load only H2 candidate keys, and return the value from
the matching bucket. `get_str`, `contains_str`, `at_str`, and `update_str` pass
their borrowed pointer/length directly to the byte hash and equality helpers.

#### Replace and update

Replacing an existing value leaves its control byte and order position intact.
The current ownership-aware value transfer/drop helpers remain responsible for
the retired and replacement slots. `dict.update` and the extract/upsert paths
perform one semantic key search and one hash.

#### Insert

An absent insertion uses the first deleted lane observed before the terminating
empty lane, otherwise the empty lane. It initializes key and value ownership,
writes the bucket, appends the bucket index to `order`, then publishes the H2
control byte and mirror. Publication order prevents an occupied control byte
from naming an uninitialized bucket.

`growth_left` is `max_occupied - (count + tombstones)`. Reusing a deleted lane
decrements `tombstones` without consuming growth budget. Inserting into an empty
lane decrements `growth_left`; when an empty lane is required and the budget is
zero, insertion rebuilds or grows before publication.

#### Remove

Successful removal transfers or duplicates the returned value exactly as the
current extract ABI requires, drops the key root, clears both slots, marks the
control byte deleted, and removes the bucket index from `order`. Remaining order
entries retain their sequence. Missing removal changes nothing.

A tombstone-pressure rebuild occurs when deleted buckets would consume the
reserved empty fraction or when measured probe-length limits are exceeded. The
rebuild may keep the same bucket count when live load fits.

#### Grow and rebuild

Growth allocates a new table, then visits the old `order` vector from first to
last. Each entry is moved or duplicated according to the existing uniqueness
proof, inserted into its new bucket, and appended to the new order vector. The
new table has no tombstones. Even a same-size tombstone rebuild writes a fresh
control/bucket/order allocation; it never rewrites the published table in place.
The root is replaced only after every owning slot and control byte is valid.

#### Iteration and projection

`keys`, `values`, and `pairs` walk `order` and load each named bucket. Lookup,
contains, replacement, and hit updates never read `order`. Rendering,
reflection, and higher-level stdlib methods continue to compose over these
projections.

### 8. Ownership and reclamation

Buckets retain the current universal key/value slots in this RFC. Existing
type-directed leaf bias, `slot_take_or_dup`, `leaf_dup`, `leaf_drop`, capacity
tokens, RC floor, and copy-on-write rules continue to govern their contents.
`__witchy_owncap` remains a capacity-token result/local; it is not a refcount
operation.

Control bytes and order indices are non-owning metadata. They never retain a
key, value, reference, or capability. Copying shared storage duplicates owning
slots before publication. Moving unique storage transfers each slot once and
leaves no second owning copy. Every grow, rebuild, failed insertion, trap, and
remove path has an explicit initialized-slot count for cleanup.

The new allocation participates in the existing RC/reuse system. Acceptance
measures requested bytes, allocation count, peak transient bytes during grow,
retained high-water capacity, and remove/reinsert churn; lower lookup latency
does not excuse unbounded retained growth.

### 9. Interpreter and stable-semantics convergence

The interpreter keeps its ordinary source-level dictionary model. It implements
the same insertion, replacement, removal, projection, and equality results
without control bytes, bucket capacity, H2, SIMD, or Wasm allocation details.

The Swiss representation and SIMD group scan are pure compiled-Wasm
optimizations under RFC-0135. Their checks compare independent expected output,
the interpreter, the current scalar Wasm index, scalar Swiss Wasm, and SIMD
Swiss Wasm. Backend agreement alone is not correctness evidence.

Because this RFC adds no source feature, it carries no interpreter debt and no
new `comptime` boundary.

### 10. Scope boundaries

The existing public surface remains the implementation target:

- `Dict(k, v)` continues to require `k: Eq` where comparison is needed;
- `dict.insert`, `update`, `remove`, `get`, `get_or`, `at`, `contains_key`,
  `keys`, `values`, `pairs`, and slice-query variants retain their signatures;
- `Set(a)` continues to use its current sealed, list-backed implementation and
  insertion-order projection; and
- normal-mode and opt-mode source programs use the same APIs they use now.

Borrowed `Entry` handles require a separate language/API proposal using the
implemented `&'a T` / `&'a mut T` syntax, declared lifetime relations, and
normal-mode boundaries. Per-type unboxed bucket layouts require a separate
layout and ownership proof. Neither is hidden inside this representation RFC.

## Measurement and acceptance

### Reproducible evidence artifact

Implementation adds a checked-in harness or documented command sequence that
records:

- source revision and active `WITCHY_OPT` schema;
- release compiler and runtime versions;
- CPU, architecture, OS, and SIMD target capability;
- workload parameters and result checksum;
- warmup policy and every raw timing sample;
- median, min/max, and a 95% confidence interval for the relative change;
- allocation count/bytes, transient grow peak, and retained heap capacity;
- hashes, control groups, H2 candidates, full key comparisons, tombstone
  rebuilds, and grow counts; and
- emitted-Wasm confirmation of scalar versus SIMD group scans.

Before/after commands alternate configurations on the same host. Setup and
compilation stay outside the kernel clock. Whole-process wall time is reported
separately. Go comparison uses the same input and host but remains contextual.

### Workload matrix

The focused matrix covers:

1. live sizes `0, 1, 15, 16, 17, 64, 1_000, 65_536`;
2. successful lookup, unsuccessful lookup, replacement, new insertion,
   remove/reinsert churn, and full ordered projection;
3. hit ratios `0%, 50%, 100%`;
4. sequential, uniformly distributed, skewed-hot, shared-prefix string, and
   deliberately H2-colliding keys;
5. `Int`, `String`, `Bytes`, borrowed string queries, and a compound/custom `Eq`
   fallback control;
6. unique in-place and shared copy-on-write roots; and
7. `dict_count`, `word_count`, and `knucleotide` as complete public workloads.

Every benchmark consumes and checks its result. A microbenchmark may attribute
the mechanism; only the complete workload supports a workload claim.

### Promotion thresholds

The optimized representation ships only when all of the following hold on the
recorded reference host:

1. every correctness and differential row below is green;
2. the geometric-mean kernel time across the three protected complete
   workloads (`dict_count`, `word_count`, and `knucleotide`) improves by at
   least 5%, and at least one protected workload improves by at least 10%; the
   full focused micro matrix remains attribution/control evidence rather than
   entering this geometric mean;
3. no protected kernel or wall workload has a statistically significant
   regression greater than 2%;
4. ordered projection is reported separately and does not regress by more than
   5% without an accepted trade-off;
5. bytes per live entry at 7/8 load do not exceed the current dense-plus-index
   representation by more than 10%;
6. remove/reinsert churn has bounded retained heap growth; and
7. the SIMD release artifact contains the intended vector group scan while the
   scalar artifact contains no vector instruction.

If variance makes a threshold indeterminate, the run count increases. If the
whole-workload gain does not survive while the micro-kernel improves, the RFC
does not promote the representation on that evidence.

### Acceptance ledger

| Contract | Required evidence |
|---|---|
| insertion order | independent expected results covering insert, replace, remove, reinsert, grow, `keys`, `values`, `pairs`, Show, and reflection |
| lookup correctness | hit/miss/collision fixtures over every supported scalar key mode and compound fallback |
| probe termination | full-table boundary, deleted-before-empty, wraparound mirror, and maximum-load tests |
| hash coherence | owned String and borrowed string-slice queries produce the same hash and equality result; Bytes hashing retains typed equality |
| ownership | unique move, shared copy, displaced value, removal, grow, rebuild, trap cleanup, and RC/reuse counters |
| scalar/SIMD equivalence | expected result equals current scalar index, scalar Swiss, and SIMD Swiss output |
| interpreter/Wasm | focused differential fixtures plus exact expected output; interpreter contains no Swiss carrier |
| optimization lever | every registered lever changes emitted code and passes the all/none/minus-one sweep |
| performance | raw matched samples, intervals, counters, allocation evidence, and promotion thresholds above |
| portability | Wasmtime and browser Wasm validation/run with SIMD; scalar target validation/run without SIMD |
| existing semantics | full dictionary, equality, Show, reflection, fuzz, corruption, and semantic-conformance suites |

## Implementation plan

### Phase 0: Baseline and attribution

1. Refresh the three public benchmarks and the focused operation matrix on the
   implementation base revision.
2. Add deterministic counters, following [RFC-0030](0030-perf-correctness-infra.md),
   for hashes, probe groups, H2 candidates, key comparisons, rebuilds, grows,
   index bytes, and `order_bytes_moved` (including remove search/shift and
   projection copies).
3. Implement and measure the vectorized-current-32-bit-index control row over
   the same matrix. It is a required Phase-0 control, not an optional
   afterthought.
4. Record a function-level profile showing the current index probe and equality
   contribution to each protected workload.

No representation change begins with an unmeasured whole-workload ceiling.

### Phase 1: Scalar Swiss representation

1. Add the header/control/bucket/order layout in
   `crates/witchy-wir/src/wir_helpers/dict/`.
2. Implement scalar group probing, one-hash `ProbeResult`, insert, replace,
   update, remove, grow, rebuild, and ordered projections.
3. Retain the current dense representation for compound/custom-`Eq` key modes.
4. Add layout, overflow, mirror, tombstone, and probe-sequence unit tests.

### Phase 2: Ownership integration

1. Route all owning slot transitions through the existing generic ownership and
   reclamation helpers.
2. Cover unique, shared, forced-copy, remove/extract, grow, and failure cleanup.
3. Prove bounded churn and preserve all no-copy diagnostics.

No new per-method `*_cap` helper or `self_*` recognizer is introduced.

### Phase 3: SIMD group scan

1. Reuse RFC-0140's WIR `v128` operations for H2 and empty masks.
2. Keep the scalar implementation over the identical representation.
3. Add emitted-Wasm shape tests and target-capability selection.
4. Register real optimization consumers and bump the optimization schema.

### Phase 4: Differential and performance gate

1. Run the acceptance ledger and de-optimization matrix.
2. Run the matched workload and allocation matrix.
3. Inspect the post-change profile for moved costs, especially hashing, ordered
   projection, grow, and RC traffic.
4. Promote only if the whole-workload thresholds pass; otherwise retain the
   evidence and defer or reject the representation.

### Phase 5: Documentation and evidence

Update architecture and performance documentation with measured results and
the final representation. Standard-library documentation changes only if an
observable contract changes; this RFC specifies none.

## Alternatives

### Keep dense ordered entries and replace only the hidden index

This is the smallest representation change and resembles an ordered map with a
Swiss index into dense entries. Iteration stays cache-dense and ownership churn
changes little, but every successful lookup follows a bucket-to-entry
indirection. It is the primary fallback if flat buckets fail the ownership,
memory, or whole-workload gates.

### Drop the insertion-order contract

Flat physical bucket iteration would remove the order vector. It would also
change `keys`, `values`, `pairs`, Show, reflection, effect order, and programs
that select an element from those Lists. Specifying bucket order would freeze
hash/probe/growth details into the language; leaving it unspecified would allow
backend-dependent program answers. A separate stable-semantics RFC may propose
that trade with migration and independent expected behavior. It is not an
implicit consequence of this optimization.

### Canonically sort projections

Sorting would detach iteration from bucket layout but requires `Ord`, while
`Dict` accepts `Eq` keys. A canonical serialization tie-breaker would add a new
contract for custom nominal types and custom equality. It is broader and makes
projection more expensive.

### Add a public `Hash` trait now

This could enable compound-key indexing, but custom equality/hash coherence is
a language contract, not a runtime detail. Derivation, heterogeneous queries,
seeding, and adversarial input need their own design.

### Add borrowed `Entry` handles now

Entry handles may remove repeated user-level searches in APIs not already
covered by `dict.update` and extract/upsert fusion. They are a stable opt-mode
reference surface with lifetime and normal-mode consequences. They do not
belong in a representation-only optimization.

### Add unboxed per-type buckets now

Typed scalar buckets may remove slot conversions and reduce width for some
shapes. Arbitrary key/value layouts also introduce layout descriptors,
alignment, root traversal, move/drop, generic ABI, and code-size questions.
Swiss probing can be measured independently first.

### Vectorize the current 32-bit index only

Scanning four current index buckets at a time avoids a representation rewrite,
but it provides no H2 filter and still loads each candidate key through the
dense-entry indirection. It is the required low-cost Phase-0 control row,
measured with the deterministic counters from [RFC-0030].

### Keep the current implementation

The current index is correct, insertion-ordered, and already fast enough to
beat Go on `dict_count` in the checked-in snapshot. It remains the right answer
if the proposed representation does not clear its whole-workload and complexity
bar.

## Drawbacks

- The order vector costs four bytes per capacity slot and adds work to new-key
  insertion, removal, growth, and ordered projection.
- Flat buckets make insertion-order projection less cache-dense than the current
  entry array.
- Control metadata, tombstone policy, mirrored tails, and two probe paths add
  WIR and testing complexity.
- Compound/custom-`Eq` keys keep their current linear lookup cliff.
- The fixed internal hash remains unsuitable as a HashDoS defense.
- SIMD gains depend on the host compiler and architecture; vector instructions
  do not guarantee a speedup.
- A table grow temporarily holds old and new storage, increasing transient peak
  memory.

## Prior art

- [RFC-0030](0030-perf-correctness-infra.md) defines Witchy's deterministic
  counter and differential-measurement foundation used by Phase 0.
- [Abseil Swiss Tables](https://abseil.io/about/design/swisstables) establish the
  H1/H2 split, one-byte control metadata, group matching, equality confirmation,
  and empty-versus-deleted probe rule.
- [Rust `hashbrown`](https://github.com/rust-lang/hashbrown) is a portable
  SwissTable implementation with scalar/SIMD group abstractions and explicit
  allocation, growth, and tombstone machinery.
- [IndexMap](https://github.com/indexmap-rs/indexmap) separates hash lookup from
  deterministic insertion order using an index into ordered entries. Witchy's
  current representation is closer to this family; this RFC instead keeps
  order metadata off the flat-bucket lookup path.
- RFC-0140 supplies Witchy's WIR vector operations and original control-probe
  sketch. This RFC adopts its `0x80` empty / `0xFE` deleted encoding and provides
  the missing complete dictionary, ownership, fallback, iteration, and evidence
  contract.
