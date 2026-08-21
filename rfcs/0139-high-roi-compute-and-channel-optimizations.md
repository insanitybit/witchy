---
rfc: 0139
title: "High-ROI compute, heterogeneous dictionary lookup, and async channel optimizations"
status: proposed
created: 2026-08-21
related:
  - "0017 (codegen performance constant factors)"
  - "0029 (performance tier contract)"
  - "0034 (closing the compute gap - codegen and runtime levers)"
  - "0055 (generic typed channels with erased runtime executor)"
  - "0112 (borrowed aggregate types)"
  - "0122 (uniform borrow relations and checked reference lifetime proof)"
  - "0129 (concurrency tasks and deterministic channels)"
  - "0134 (codegen and compute optimization roadmap)"
tracking: "Proposes four high-ROI performance tracks: Equivalent heterogeneous dictionary lookups, specialized 2-way select fast-path, inductive loop BCE, and templated string interpolation"
---

# RFC-0139: High-ROI compute, heterogeneous dictionary lookup, and async channel optimizations

## Summary

Following the completion of zero-allocation string slicing (`&'a str` / `&'a [T]`, [RFC-0122](0122-uniform-borrow-relations.md)), Witchy outperforms native Go in 5 major computational benchmarks (`expr_eval` 3.77x, `binary_trees` 1.45x, `dict_count` 1.31x, `record_build` 1.30x, `list_sum` 1.25x) and matches native Go floating-point math within 5% (`mandelbrot` 1.05x).

However, four distinct architectural bottlenecks cause Witchy to lag behind Go in specific workload domains:
1. **Async Channel Multiplexing (`select_fanin` — 271.9x gap)**: Every `chan.select(a, b).await` allocates a temporary channel ID list `[ia, ib]`, a trampoline lambda closure, CPS continuation frames, and `Option((Int, __Msg))` enum envelopes.
2. **Dictionary Key Materialization (`knucleotide` — 3.11x gap)**: Slices like `&'a str` cannot query `Dict(String, V)` without cloning into owned `String` objects due to the lack of heterogeneous key comparison.
3. **Redundant Array Bounds Checks (`fannkuch` — 3.38x gap, `nsieve` — 2.77x gap)**: Monotonic induction variable loops execute dynamic bounds checks (`i32.ge_u` / trap branches) on every array index access even when index bounds are provably invariant.
4. **Intermediate String Interpolation Allocations (`word_count` — 2.74x gap)**: Formatted strings like `f"word{i % 1000}"` lower to separate `int_to_string` and `string.concat` allocations per loop iteration.

This RFC specifies concrete designs for all four optimization tracks, including the formal `Equivalent(k)` trait contract that avoids the decade-old composite key borrowing trap present in Rust's `Borrow<Q>`.

---

## Empirical Benchmark Baseline

The full 17-benchmark suite measured against equivalent native Go 1.22 on both the in-process monotonic **kernel** clock and the end-to-end **wall** clock (`benchmarks/baseline.md`, commit `97d15a4d`):

| Benchmark | Kernel Witchy (ms) | Kernel Go (ms) | Kernel vs Go | Wall Witchy (ms) | Wall Go (ms) | Dominant Workload Characteristic |
|---|---:|---:|---:|---:|---:|---|
| **`expr_eval`** | **12.7** | **47.9** | **0.27x** (3.77x faster) | 82.0 | 63.9 | AST evaluation, pattern matching, recursion |
| **`binary_trees`** | **67.4** | **97.8** | **0.69x** (1.45x faster) | 144.2 | 115.6 | Deep tree allocation, traversal, RC throughput |
| **`dict_count`** | **25.5** | **33.4** | **0.76x** (1.31x faster) | 102.4 | 55.1 | String dictionary lookups and upsert updates |
| **`record_build`** | **1.5** | **1.9** | **0.77x** (1.30x faster) | 67.8 | 8.5 | Mutable user record field-path list accumulation |
| **`list_sum`** | **8.2** | **10.3** | **0.80x** (1.25x faster) | 86.8 | 32.3 | In-place list growth and sequential iteration |
| **`mandelbrot`** | **36.3** | **34.6** | **1.05x** (within 5%) | 114.8 | 52.4 | Floating-point complex arithmetic loops |
| **`collatz`** | **204.9** | **145.3** | **1.41x** | 288.8 | 165.3 | Integer arithmetic and branch prediction |
| **`fib`** | **31.7** | **20.0** | **1.59x** | 106.9 | 32.2 | Deep call-stack recursion |
| **`closure_calls`** | **3.8** | **2.2** | **1.72x** | 83.1 | 11.9 | Higher-order functions and closure invocation |
| **`list_index`** | **5.1** | **2.6** | **1.98x** | 78.0 | 10.5 | Random list indexing and bounds checks |
| **`loop_sum`** | **50.6** | **25.5** | **1.98x** | 121.2 | 34.8 | Tight integer increment loops |
| **`word_count`** | **139.8** | **51.0** | **2.74x** | 235.8 | 73.6 | Text formatting, string hashing, dict updates |
| **`nsieve`** | **8.4** | **3.0** | **2.77x** | 100.6 | 8.3 | Bitwise / boolean sieve array mutation |
| **`knucleotide`** | **53.5** | **17.2** | **3.11x** | 155.4 | 48.6 | Zero-allocation string slicing + k-mer hashing |
| **`fannkuch`** | **447.7** | **132.6** | **3.38x** | 523.8 | 154.7 | Permutation indexing and array reversals |
| **`chan_throughput`** | — | — | — | 119.1 | 22.2 | Multi-task actor channel message passing |
| **`select_fanin`** | **24.9** | **0.1** | **271.91x** | 520.3 | 7.3 | Async multi-channel select multiplexing |

---

## Track 1: Heterogeneous `Equivalent(k)` Dictionary Lookups

### 1.1 The Composite Key Borrowing Trap

In systems like Rust's `std::collections::HashMap`, heterogeneous lookups rely on `Borrow<Q>`:
```rust
trait Borrow<Q> {
    fn borrow(&self) -> &Q;
}
```
Because `borrow(&self)` returns a *reference* `&Q`, `Q` must physically exist in contiguous memory inside `self`. This works for scalar references like `&String -> &str`, but fails completely for composite keys:
* A `(String, String)` stored key has layout `[ptr, len, cap, ptr, len, cap]`.
* A query pair `(&str, &str)` is a tuple of two fat pointers `[ptr, len, ptr, len]`.
* `(&str, &str)` does not exist contiguously inside `(String, String)`.

Consequently, standard `Borrow` cannot borrow `(String, String)` as `(&str, &str)`, forcing developers to allocate owned `String` pairs just to query a map.

### 1.2 The `Equivalent(k)` Trait Specification

Witchy solves this by decoupling the query type from reference subtyping via the `Equivalent(k)` trait in `std/cmp.witchy`:

```witchy
// std/cmp.witchy

// Trait for a query value `q` that can be compared for equivalence against a stored key `k`.
// Invariant: If q.equivalent(k), then hash(q) == hash(k).
pub trait Equivalent(k):
    fn equivalent(self: &'a Self, key: &'b k) -> Bool
```

### 1.3 Implementations across Scalars, Slices, Tuples, and Records

```witchy
// 1. Reflexive equivalence for any Eq type:
impl Equivalent(a) for a where a: Eq:
    fn equivalent(self: &'a a, key: &'b a) -> Bool:
        self == key

// 2. Borrowed string slice equivalent to owned String:
impl Equivalent(String) for str:
    fn equivalent(self: &'a str, key: &'b String) -> Bool:
        self == key.as_str()

// 3. Borrowed array slice equivalent to owned List:
impl Equivalent(List(t)) for [t] where t: Eq:
    fn equivalent(self: &'a [t], key: &'b List(t)) -> Bool:
        self == key.as_slice()

// 4. Composite tuple equivalence (zero-allocation composite lookup):
impl Equivalent((k1, k2)) for (q1, q2) where q1: Equivalent(k1), q2: Equivalent(k2):
    fn equivalent(self: &'a (q1, q2), key: &'b (k1, k2)) -> Bool:
        let (q_a, q_b) = self
        let (k_a, k_b) = key
        q_a.equivalent(k_a) && q_b.equivalent(k_b)
```

### 1.4 Heterogeneous Dictionary API

The query functions in `std/dict.witchy` generalize over any `q: Hash + Eq + Equivalent(k)`:

```witchy
// std/dict.witchy

pub fn get(d: Dict(k, v), query: &'a q) -> Option(v) 
where 
    q: Hash + Eq + Equivalent(k):
    dict.get(d, query)

pub fn contains_key(d: Dict(k, v), query: &'a q) -> Bool 
where 
    q: Hash + Eq + Equivalent(k):
    dict.contains_key(d, query)

// Single-lookup in-place upsert:
// On Hit: zero allocations (reads/updates value in-place).
// On Miss: calls to_owned() exactly once to insert the new key.
pub fn update(
    var d: Dict(k, v), 
    query: &'a q, 
    default: v, 
    f: fn(v) -> v
) 
where 
    q: Hash + Eq + Equivalent(k) + ToOwned(k):
    dict.update(d, query, default, f)
```

### 1.5 Expected Impact
* **`knucleotide`**: Eliminates all 200,000 temporary string allocations in the inner k-mer counting loop. Expected execution time drops from **`53.5 ms`** to **`~18 ms`** (reaching parity with native Go's `17.2 ms`).
* **Routing & HTTP Parsers**: Direct zero-allocation lookups for composite routes like `dict.get(routes, (&"GET", &"/api/v1/users"))`.

---

## Track 2: Specialized 2-Way Async Channel `select` & Fast-Path Multiplexing

### 2.1 Problem Analysis (`select_fanin` — 271.9x Gap)

In `std/chan.witchy`, `chan.select(a, b)` currently expands to:
```witchy
pub fn select(a: Receiver(m), b: Receiver(m)) -> Task(Selected(m)):
    match a:
        Receiver(ia) ->
            match b:
                Receiver(ib) -> task.__channel_select([ia, ib], fn(o): select_result(o))
```
On every single iteration:
1. `[ia, ib]` creates a new heap-allocated `List(ChannelId)`.
2. `fn(o): select_result(o)` creates an allocated closure environment.
3. The CPS executor wraps the operation in a new `Task` continuation and polls via a dynamic loop.
4. The received value is boxed in `Option((Int, __Msg))` before matching into `Selected(m)`.

In Go, `select { case v := <-a: ... case v := <-b: ... }` lowers to an unboxed 2-pointer check against channel ring buffers with zero allocations.

### 2.2 Mechanism: Direct 2-Way Primitive `task.__channel_select2`

1. **Intrinsics & Runtime Hook**:
   Add compiler primitive `task.__channel_select2(ia: ChannelId, ib: ChannelId) -> Task(Option((Int, __Msg)))` avoiding list construction.
2. **Immediate Buffer Poll Fast-Path**:
   Before constructing task continuations or parking, the compiler-generated select checks whether channel `ia` has a buffered message.
   - If `ia` has a message: immediately dequeues `(0, msg)` synchronously.
   - Else if `ib` has a message: immediately dequeues `(1, msg)` synchronously.
   - Only if both are empty does it register a parking waiter on both channels.
3. **Unboxed Result Inlining**:
   Specialize `Selected(m)` variant construction in `witchy-lower` directly from the integer branch index and unboxed scalar payload.

### 2.3 Expected Impact
* `select_fanin` kernel time drops from **`24.9 ms`** to **`< 1.0 ms`** (**25x–50x speedup**).

---

## Track 3: Monotonic Loop Induction Range Proofs & Bounds Check Elimination (BCE)

### 3.1 Problem Analysis (`fannkuch` — 3.38x Gap, `nsieve` — 2.77x Gap)

In tight array-manipulation kernels:
```witchy
var i = 0
while i < n:
    xs[i] = xs[i] + 1
    i = i + 1
```
Currently, `witchy-lower` lowers every `xs[i]` access to:
```wat
local.get $i
local.get $xs
i32.load offset=0  ;; len
i32.ge_u
if
    unreachable    ;; bounds check failure trap
end
;; load/store at $xs + 4 + i * 8
```
In `fannkuch` (447 ms) and `nsieve` (8.4 ms), billions of redundant `i32.load offset=0` and `i32.ge_u` instructions execute in tight loops where `0 <= i < n <= xs.len` is an induction invariant.

### 3.2 Inductive Range Analysis in `witchy-lower`

Extend `crates/witchy-lower/src/analysis.rs` with an induction range verifier:
1. **Loop Canonicalization**: Identify monotonic counters `var i = 0; while i < bound: ... i = i + 1`.
2. **Length Guard Hoisting**: If `bound <= list.length(xs)` can be proven at loop entry, mark all `xs[i]` accesses inside the loop body as **`Unchecked`**.
3. **Unchecked Lowering**:
   Lower `Unchecked` indexing directly to:
   ```wat
   local.get $xs
   local.get $i
   i32.const 3
   i32.shl
   i32.add
   i64.load offset=4
   ```
   Completely removing the length load, comparison, and branch.

### 3.3 Expected Impact
* **`fannkuch`**: Runtime drops from **`447.7 ms`** to **`~180 ms`** (~2.5x speedup).
* **`nsieve`**: Runtime drops from **`8.4 ms`** to **`~3.5 ms`** (~2.4x speedup, near Go's 3.0 ms).
* **`list_index` & `loop_sum`**: ~1.5x–1.8x speedup.

---

## Track 4: Template-Specialized String Interpolation Buffer Reuse

### 4.1 Problem Analysis (`word_count` — 2.74x Gap)

In `benchmarks/word_count.witchy`:
```witchy
let w = f"word{i % 1000}"
```
This is currently lowered into:
```witchy
let _tmp1 = int_to_string(i % 1000)
let _tmp2 = string.concat("word", _tmp1)
```
Across 1,000,000 iterations, 2,000,000 heap string buffers are allocated, copied, and released.

### 4.2 Single-Pass Interpolation Lowering

In `crates/witchy-lower/src/codegen/expr_lower.rs`:
1. Recognize format string templates `f"prefix{int_expr}suffix"`.
2. Calculate the maximum buffer size on the stack: `prefix.len + 20 (max i64 digits) + suffix.len`.
3. Allocate a single string buffer with `bump_alloc(total_len)`.
4. Copy `prefix`, format `int_expr` into ASCII digits in-place, copy `suffix`, and store the final length word.
5. Zero intermediate allocations.

### 4.3 Expected Impact
* `word_count` kernel time drops from **`139.8 ms`** to **`~60 ms`** (2.3x speedup, matching Go's `51 ms`).

---

## Implementation Ledger & Acceptance Criteria

| Track | Component | Target Benchmarks | Acceptance Criteria |
|---|---|---|---|
| **Track 1** | `Equivalent(k)` Trait & `dict` Lookup | `knucleotide` | `dict.get` and `dict.update` accept `&'a str` and `(&'a str, &'b str)` with 0 heap allocations on hits. `knucleotide` kernel time <= 22 ms. |
| **Track 2** | 2-Way `chan.select` Fast-Path | `select_fanin` | `select_fanin` kernel time drops below 2.0 ms. No list or closure allocation in 2-way select loops. |
| **Track 3** | Loop Induction BCE | `fannkuch`, `nsieve`, `list_index` | `fannkuch` kernel time <= 200 ms. `nsieve` kernel time <= 4.0 ms. Compiled WAT shows no `i32.ge_u` bounds check in proven loops. |
| **Track 4** | Templated Interpolation | `word_count` | `f"word{n}"` performs exactly 1 string allocation per iteration instead of 2. `word_count` kernel time <= 70 ms. |
