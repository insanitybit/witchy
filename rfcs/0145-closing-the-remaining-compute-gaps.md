---
rfc: 0145
title: "Closing the Remaining Compute Gaps: Zero-Allocation Async Select, Leaf Function Inlining, 1-Byte Monomorphized Arrays, Inline Bounds Verification, and Stack-Buffered Lookup Views"
status: proposed
created: 2026-08-22
related:
  - "0017 (codegen performance constant factors)"
  - "0027 (packed layouts and sroa)"
  - "0029 (performance tier contract)"
  - "0034 (closing the compute gap - codegen and runtime levers)"
  - "0055 (generic typed channels with erased runtime executor)"
  - "0122 (uniform borrow relations and checked reference lifetime proof)"
  - "0129 (concurrency tasks and deterministic channels)"
  - "0134 (codegen and compute optimization roadmap)"
  - "0139 (high-roi compute, heterogeneous dictionary lookup, and async channel optimizations)"
  - "0140 (wasm simd and relaxed simd acceleration)"
  - "0143 (hybrid swisstable dictionary architecture)"
  - "0144 (hybrid list and string architecture)"
tracking: "Specifies five targeted compiler and runtime optimizations to eliminate the remaining performance gaps between Witchy and native Go across async channel multiplexing, small recursive function inlining, 1-byte boolean/byte arrays, direct inline bounds checking, and zero-allocation stack-buffered lookup views."
---

# RFC-0145: Closing the Remaining Compute Gaps

## Summary

Following the delivery of RFC-0139 and RFC-0144, Witchy outperforms native Go in 7 major benchmarks (`expr_eval` 3.18x, `mandelbrot` 1.68x, `binary_trees` 1.51x, `record_build` 1.36x, `dict_count` 1.31x, `loop_sum` 1.16x, `closure_calls` 1.11x) and achieved massive multi-thousand-fold improvements on data-intensive workloads (`knucleotide` 3,676x faster, `word_count` 8.5x faster).

However, empirical analysis of the remaining benchmark suite identifies **5 distinct architectural bottlenecks** where Witchy still lags behind native Go:

1. **Async Channel Multiplexing Overhead (`select_fanin` — 265.9x gap)**: `chan.select(a, b).await` dynamically allocates a channel ID list `[ia, ib]`, a trampoline lambda closure, CPS continuation frames, and `Option((Int, Msg))` enum envelopes per iteration.
2. **Call-Stack Frame Overhead on Small Functions (`fib` — 1.38x gap, `collatz` — 1.10x gap)**: Un-inlined leaf functions cross WebAssembly function call boundaries without cross-call register residency.
3. **8-Byte Slot Boxing for Boolean/Byte Arrays (`nsieve` — 2.19x gap)**: `List(Bool)` and `List(Byte)` allocate 64 bits per element (6.4 MB for 800k items), exceeding CPU L1/L2 cache limits compared to Go's 1-byte `[]bool` (800 KB).
4. **Out-of-Line Function Call on Random Index Reads (`list_index` — 1.91x gap)**: Non-monotonic index lookups make a full function call to the `$list_at` runtime helper instead of executing inline machine checks.
5. **Transient String Allocation in Key Lookup Position (`word_count` — 2.31x gap)**: Interpolated strings like `d[f"word{i % 1000}"]` allocate a heap string even though the key is only inspected temporarily during map probing.

This RFC provides concrete designs to eliminate all five bottlenecks.

---

## Track 1: Zero-Allocation 2-Way and 3-Way Async Channel Select Fast-Paths

### Current Bottleneck
In `select_fanin`, multi-channel multiplexing lowers to the generic N-way runtime helper `chan_select_n`. This helper takes a dynamic heap list of channel references `[a, b]`, builds a closure callback, and returns a boxed `Option((Int, Msg))` tuple. In a tight loop executing 500,000 select operations, this generates over 2,000,000 transient heap allocations.

### Technical Design
Introduce specialized **`select2`** and **`select3`** direct WIR primitives:

```wat
;; $chan_select2(ch_a: i32, ch_b: i32) -> (ready_index: i32, payload: i64)
;; 1. Poll channel A's atomic head/tail pointers on the stack.
;; 2. If empty, poll channel B.
;; 3. If both empty, park the current fiber with a stack-allocated wait-node.
;; 4. Return (0, val_a) or (1, val_b) as a multi-value pair directly in registers.
```

- **Zero List Allocation**: The channel pointers `ch_a` and `ch_b` travel directly in WebAssembly local registers.
- **Zero Continuation Closure**: The resumption point is encoded as an integer state index in the fiber control block.
- **Zero Tuple Envelope**: Emits multi-value returns `(ready_index: i32, payload: i64)` without constructing heap objects.

---

## Track 2: Small Leaf Function and Call-Tree Inlining

### Current Bottleneck
Functions like `fib` and `collatz` consist of tiny arithmetic expressions and conditional branches. Because they are emitted as separate WebAssembly functions, every recursive or inter-function call incurs frame allocation, parameter spilling, and return jumps.

### Technical Design
The AST optimizer and lowerer (`witchy-lower`) will implement a **Small-Function Inliner**:

1. **Inlining Budget**: Functions with $\le 16$ AST nodes without loops or heap allocations qualify as inline candidates.
2. **Recursive Unrolling**: For small recursive functions with literal or monotonic induction inputs (e.g. `fib(n)` where $n \le 4$), unroll into a direct branchless expression tree.
3. **Dead Argument Elimination**: When inlining into a caller where a parameter is constant, propagate the constant and fold unreachable branches before WIR generation.

---

## Track 3: Monomorphized 1-Byte Packed Storage for `List(Bool)` and `List(Byte)`

### Current Bottleneck
In `nsieve` (Sieve of Eratosthenes), an 800,000-element boolean sieve is stored in `List(Bool)`. Currently, each boolean is boxed into an 8-byte NaN slot (`ToSlot`/`FromSlot`), taking 6.4 MB of memory and causing continuous L2/L3 cache misses.

### Technical Design
Specialized unboxed memory layout for `List(Bool)` and `List(Byte)`:

1. **1-Byte Stride**:
   - Element count and capacity remain 32-bit integers in the 4-byte header.
   - Payload buffer is allocated with 1 byte per element: `size = 4 + capacity * 1`.
2. **Direct Load/Store**:
   - `list.at(flags, i)` lowers to `i32.load8_u offset=4 (base + i)`.
   - `list.set_at(flags, i, b)` lowers to `i32.store8 offset=4 (base + i, b)`.
3. **Cache Footprint**:
   - Drops memory usage from 6.4 MB to **800 KB**, fitting entirely into L2 cache and matching Go's `[]bool` memory density.

---

## Track 4: Inlined Bounds-Check Sequences for Random Reads

### Current Bottleneck
In `list_index`, random array indexing `list.at(xs, rand_i)` cannot be statically proved in-bounds by loop induction. Consequently, every read emits a function call `call $list_at (xs, rand_i)`. The function call prologue, register save/restore, and jump overhead double the read latency.

### Technical Design
When bounds check elision cannot prove invariance, emit the checked read **directly inline in the caller**:

```wat
;; Inline bounds-checked read: list.at(xs, idx)
local.get $xs
local.get $idx
;; Check: if unsigned idx >= length -> trap
local.get $idx
local.get $xs
i32.load offset=0 ;; length
i32.ge_u
if
    call $__witchy_abort_oob
end
;; In-bounds load: (xs + 4) + idx * 8
i32.const 4
i32.add
local.get $idx
i32.const 8
i32.mul
i32.add
i64.load offset=0
```

Eliminates function call overhead entirely for random reads, reducing index latency to **~1.8 ms**.

---

## Track 5: Zero-Allocation Stack-Buffered String Lookup Views

### Current Bottleneck
In `word_count`, formatted strings such as `d[f"word{i % 1000}"]` construct a temporary heap string on every loop iteration, even though the string is immediately discarded after the hash table probe.

### Technical Design
When a formatted string expression `f"..."` is passed directly into a borrowed parameter accepting `&'a str` (e.g. `dict.get_str`, `dict.update_str`):

1. **Stack Buffer Allocation**: Allocate 32 bytes on the shadow stack.
2. **In-Place Format**: Write the literal prefix and format the integer directly into the stack buffer.
3. **Pass Borrowed Slice**: Pass `(stack_ptr: i32, len: i32)` as a zero-allocation `&'a str` view.
4. **Zero Heap Churn**: Zero calls to `$rc_alloc` or `$heap` bump during the map lookup loop.

---

## Concrete Performance Goals

| Benchmark | Current Witchy (ms) | Native Go 1.22 (ms) | Target Witchy (ms) | Projected vs Go |
| :--- | :---: | :---: | :---: | :---: |
| **`select_fanin`** | 23.9 | 0.1 | **0.12** | **~1.0x (Parity with Go)** |
| **`fib`** | 32.0 | 23.3 | **18.0** | **0.77x (1.30x faster)** |
| **`collatz`** | 222.6 | 202.4 | **175.0** | **0.86x (1.16x faster)** |
| **`nsieve`** | 6.7 | 3.1 | **2.2** | **0.71x (1.41x faster)** |
| **`list_index`** | 5.0 | 2.6 | **1.8** | **0.69x (1.44x faster)** |
| **`word_count`** | 116.4 | 50.3 | **35.0** | **0.70x (1.44x faster)** |
| **`fannkuch`** | 251.0 | 129.3 | **110.0** | **0.85x (1.18x faster)** |

---

## Acceptance Criteria

1. **Async Channel Select**:
   - `chan.select(a, b)` must execute with 0 heap allocations on ready channel paths.
2. **Leaf Function Inlining**:
   - Trivial leaf functions ($\le 16$ AST nodes) called in non-escaping positions must be inlined into the caller.
3. **1-Byte Arrays**:
   - `List(Bool)` and `List(Byte)` must allocate exactly 1 byte per element in memory.
4. **Inline Bounds Check**:
   - Random `list.at(xs, i)` must inline the branch/load instructions without calling out-of-line helpers.
5. **Stack-Buffered Format Views**:
   - Suffix/prefix string interpolation in `&'a str` argument position must allocate 0 bytes in heap memory.
6. **No Semantic Regressions**:
   - 100% pass rate across the full repository test suite.

---

## Migration Plan

1. **Phase 1: Specialized Channel Select**: Implement `chan_select2` in `witchy-wir` and lower in `witchy-lower`.
2. **Phase 2: Monomorphized 1-Byte Arrays & Inline Bounds Checking**: Update `witchy-lower` stride calculation and inline load emission.
3. **Phase 3: Stack-Buffered String Formatting**: Add shadow-stack string formatting in `witchy-lower`.
4. **Phase 4: Small-Function Inliner**: Add AST/WIR inlining pass in `witchy-lower`.
5. **Phase 5: Benchmark Verification**: Verify all 17 benchmarks outperform or match Go 1.22.
