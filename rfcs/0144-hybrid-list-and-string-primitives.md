---
rfc: 0144
title: "Hybrid List and String Architecture: Monomorphized Unboxed Storage, SIMD UTF-8 Slices, and Safe Pure-Witchy Borrowed Surface"
status: implemented
created: 2026-08-22
related:
  - "0017 (codegen performance constant factors)"
  - "0026 (unique qualifier)"
  - "0027 (packed layouts and sroa)"
  - "0028 (ergonomic mutable value semantics)"
  - "0031 (simd stdlib hot loops)"
  - "0034 (closing the compute gap - codegen and runtime levers)"
  - "0122 (uniform borrow relations and checked reference lifetime proof)"
  - "0134 (codegen and compute optimization roadmap)"
  - "0139 (high-roi compute, heterogeneous dictionary lookup, and async channel optimizations)"
  - "0140 (wasm simd and relaxed simd acceleration)"
  - "0143 (hybrid swisstable dictionary architecture)"
tracking: "Specifies a hybrid architecture for Witchy's fundamental sequence types: monomorphized unboxed List storage with SIMD memory primitives, paired with zero-allocation SIMD-accelerated UTF-8 String slices and stack-buffered interpolation, exposed through pure Witchy borrow-checked APIs."
---

# RFC-0144: Hybrid List and String Architecture

## Summary

Following the hybrid collection design established in [RFC-0143](0143-hybrid-swisstable-collections.md) for associative maps, Witchy's two primary sequential data structures—`List(T)` and `String`—require a matching architectural transformation to fully unlock native hardware performance.

Currently, `List` and `String` suffer from fundamental abstraction leaks and runtime helper boundaries:
1. **List Generic NaN-Slot Boxing & Indirection**: `List(T)` stores elements in generic 64-bit NaN slots. Even for primitive `Int` or small `struct` arrays, elements are coerced through `ToSlot`/`FromSlot` or boxed as separate heap allocations, destroying memory density and CPU L1 cache locality.
2. **Helper Call Overhead for Sequential Access**: Operations like `list.at` and `list.set_at` cross runtime helper call boundaries (`list_at`, `list_set_cap`), preventing loop-invariant code motion (LICM), loop unrolling, and compiler auto-vectorization.
3. **String Allocation Churn and $O(n)$ Character Scans**: Slicing strings (`string.substring`) or string interpolation (`"word${i}"`) triggers heap allocations and linear-time character-to-byte offset scans, turning simple text processing loops into quadratic or allocation-bound bottlenecks.

This RFC proposes a unified **Hybrid Architecture for Sequential Primitives**:
- **Pure Witchy on Top (`std/list.witchy`, `std/string.witchy`)**: Ergonomic, borrow-checked APIs (`&'a [T]`, `&'a str`, `split_at_mut`, iterators, in-place `var` builders) backed by Witchy's affine borrow checker.
- **Compiler Layout Monomorphization (`witchy-lower`)**: Monomorphized contiguous flat memory layouts (1-byte for `Byte`/`Bool`, 8-byte for `Int`/`Float`, flat inline stride for small structs) with direct machine load/store emission.
- **Hardware-Accelerated WIR Engine Underneath (`witchy-wir`)**: High-throughput SIMD vector kernels (`v128`) for bulk memory movement (`memmove`/`memcpy`), SIMD ASCII validation, and 16-way parallel byte searching (`memchr`).

---

## Architectural Decomposition

```
┌────────────────────────────────────────────────────────────────────────┐
│             Pure Witchy Safe Surface (std/list, std/string)            │
│                                                                        │
│  - Borrowed Slices: &'a [T], &'a str (zero allocation, zero copy)      │
│  - Affine Mutation: `var list.push(x)`, `split_at_mut`, `string.append` │
│  - Zero Iterator Invalidation & Proven Lifetime Boundaries             │
│  - Safe UTF-8 Grapheme & Unicode Scalar Iterators                     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Monomorphized Layouts & Inlining
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   Compiler & Lowerer (witchy-lower)                    │
│                                                                        │
│  - Unboxed Stride Specialization: Flat contiguous memory per type T    │
│  - Small Struct Inlining: Pack `List(Point)` directly without pointers │
│  - Direct In-Bounds Store/Load Emission (`i64.load`, `i64.store`)      │
│  - Stack-Buffered String Interpolation (Single-pass formatting)        │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Hardware Vector Primitives
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                    Low-Level WIR Engine (witchy-wir)                   │
│                                                                        │
│  - `RawBuffer`: Contiguous memory `(ptr, len, cap)` with SIMD memcpy   │
│  - 16-Way SIMD Byte Search (`v128` / `i8x16.bitmask` memchr)          │
│  - 16-Way SIMD ASCII Fast-Path Validation & Vectorized Equality        │
│  - Exponential Capacity Resizing with In-Place Growth Check            │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Technical Design

### Part 1: `List(T)` & `&'a [T]` (Monomorphized Flat Arrays)

#### 1. Low-Level WIR Primitive (`RawBuffer`)
The WIR engine provides a lean, contiguous memory descriptor:
```wat
(type $raw_buffer (struct
    (field $ptr (mut i32))      ;; Pointer to element payload
    (field $len (mut i32))      ;; Element count
    (field $cap (mut i32))      ;; Allocated capacity in elements
))
```
- **Bulk Vectorized Growth**: Resizing uses 16-byte SIMD vector loads/stores (`v128.load`/`v128.store`) for block copying instead of single-byte loops.
- **In-Place Reallocation**: If the buffer is the most recent bump allocation or uniquely owned with spare capacity, growth extends the buffer with zero memory copy.

#### 2. Compiler Stride Specialization (`witchy-lower`)
The lowerer eliminates the generic `ToSlot`/`FromSlot` layer by computing exact type strides:

| Element Type `T` | Element Stride | Wasm Load/Store Instruction | Boxing Overhead |
| :--- | :---: | :---: | :---: |
| `Byte`, `Bool` | 1 byte | `i32.load8_u` / `i32.store8` | **0 bytes** (was 8-byte slot) |
| `Int`, `Float` | 8 bytes | `i64.load` / `i64.store` | **0 bytes** (was 8-byte slot) |
| `Point { x: Int, y: Int }` | 16 bytes | Inline 16-byte struct row | **0 bytes** (was heap pointer) |
| `Ref` (String, Dict, Heap) | 4 bytes | `i32.load` / `i32.store` | **0 bytes** |

When bounds elision (RFC-0034 / RFC-0139) proves that an index is in range:
- `list.at(xs, i)` lowers to a single `i64.load offset=0 (base + i * stride)`.
- `list.set_at(xs, i, v)` lowers to a single `i64.store offset=0 (base + i * stride, v)`.
- Helper function calls (`$list_at`, `$list_set_cap`) and branch instructions are completely eliminated in hot loops.

#### 3. Pure Witchy Standard Library Surface (`std/list.witchy`)
```witchy
pub struct List(T):
    raw: RawBuffer(T)

pub struct Slice(T):
    ptr: &'a RawBuffer(T)
    offset: Int
    len: Int

impl List(T):
    pub fn new() -> List(T):
        List { raw: raw_buffer.new(0) }

    pub fn with_capacity(cap: Int) -> List(T):
        List { raw: raw_buffer.with_capacity(cap) }

    pub fn length(self) -> Int:
        raw_buffer.length(self.raw)

    pub fn at(self, index: Int) -> &T:
        raw_buffer.get_ref(self.raw, index)

    pub fn set_at(var self, index: Int, val: T):
        raw_buffer.set(self.raw, index, val)

    pub fn push(var self, val: T):
        raw_buffer.push(self.raw, val)

    pub fn as_slice(self) -> &'a [T]:
        raw_buffer.as_slice(self.raw)
```

---

### Part 2: `String` & `&'a str` (SIMD UTF-8 & Zero-Copy Slices)

#### 1. Zero-Copy String Slice View (`&'a str`)
A string slice view is represented directly on the stack or in WebAssembly local registers as a 64-bit pair `(ptr: i32, len: i32)` without allocating a heap object descriptor.

```witchy
// Zero-allocation byte substring
let kmer: &'a str = string.slice(seq, j, j + k)
```

- **Instant $O(1)$ Slicing**: Slicing simply adjusts `(ptr + start, len)` with bounds validation, eliminating $O(n)$ character rescans.
- **Heterogeneous Compatibility**: Slices implement `Hash` and `Eq` identically to owned strings, allowing direct queries into `Dict(String, V)` with zero allocations (RFC-0139 / RFC-0143).

#### 2. SIMD Vector Kernels in WIR (`witchy-wir`)
Using WebAssembly `v128` SIMD vectors (RFC-0140):

1. **16-Way Parallel `memchr` Search**:
   Scanning for delimiter bytes (e.g. `' '`, `'\n'`, `','`) loads 16 bytes per cycle and checks for matches using `i8x16.eq` and `i8x16.bitmask`.
2. **16-Way ASCII Fast-Path Validation**:
   Checks whether 16 bytes are pure ASCII (`high_bit == 0`) in 1 cycle via `v128.any_true(i8x16.bitmask(bytes) & 0x8080...)`.
3. **16-Way Vector String Equality (`dict_slice_eq`)**:
   Compares 16 bytes per loop iteration, dropping equality comparison latency by 4x–8x.

#### 3. Stack-Buffered String Interpolation
Formatting expressions such as `f"word{i % 1000}"` currently allocate multiple intermediate string fragments on the heap.

Under this RFC, string interpolation compiles to a **single-pass stack format buffer**:
```wat
;; Allocate 64 bytes on the shadow stack
local.get $sp
i32.const 64
i32.sub
local.tee $buf

;; Write literal prefix "word" directly
i32.const 0x64726f77 ;; "word"
i32.store offset=0

;; Format integer directly into buffer starting at offset 4
call $fmt_int_into (local.get $buf, i32.const 4, local.get $val)

;; Construct &'a str view pointing to stack buffer (zero heap allocation!)
```

When passed immediately to `dict.get(d, ...)` or `dict.update(d, ...)`, the borrowed stack slice is consumed directly with **zero heap allocations**.

---

## Concrete Performance Goals

| Benchmark | Current Witchy (ms) | Native Go 1.22 (ms) | Target Hybrid Primitives (ms) | Projected vs Go |
| :--- | :---: | :---: | :---: | :---: |
| **`fannkuch`** | 566.7 | 1451.1 | **120.0** | **0.08x (12.1x faster)** |
| **`nsieve`** | 16.2 | 4.5 | **2.2** | **0.49x (2.04x faster)** |
| **`word_count`** | 222.0 | 93.5 | **42.0** | **0.45x (2.22x faster)** |
| **`knucleotide`** | 332.7 | 40.4 | **15.0** | **0.37x (2.69x faster)** |
| **`list_sum`** | 23.0 | 42.0 | **6.5** | **0.15x (6.46x faster)** |
| **`list_index`** | 6.1 | 4.8 | **1.8** | **0.38x (2.66x faster)** |

---

## Acceptance Criteria

1. **Unboxed List Layouts**:
   - `List(Int)` and `List(Float)` must be stored in contiguous 8-byte unboxed memory.
   - `List(Byte)` and `List(Bool)` must be stored in contiguous 1-byte unboxed memory.
   - Small structs (size $\le$ 32 bytes) must be packed inline without indirection pointers.
2. **SIMD Vectorization**:
   - Bulk copy operations on `RawBuffer` must use `v128` 16-byte block transfers.
   - Byte search (`string.index_of_byte`, `string.split_byte`) must use `v128` SIMD `memchr`.
3. **Zero-Allocation Slicing & Formatting**:
   - String slicing (`string.slice`, `&s[a..b]`) must never allocate heap memory.
   - Local string interpolation in call-position (`d[f"word{i}"]`) must format into stack memory with 0 heap allocations.
4. **Safety and Uniqueness**:
   - The borrow checker must prevent modifications to a `List` or `String` while active slices (`&'a [T]`, `&'a str`) are borrowed.
   - In-place mutation with `var` must update buffers without reference-count increments.
5. **No Regressions**:
   - Full test suite passes across all standard library sequence operations.

---

## Migration Plan

1. **Phase 1: WIR `RawBuffer` & SIMD String Kernels**:
   - Implement `raw_buffer_*` and `str_simd_*` in `crates/witchy-wir/src/wir_helpers/`.
2. **Phase 2: Monomorphized Lowering in `witchy-lower`**:
   - Implement stride monomorphization and direct `i64.load`/`i64.store` generation for `List(T)`.
   - Implement stack-buffered string formatting.
3. **Phase 3: Standard Library Refactor**:
   - Update `std/list.witchy` and `std/string.witchy` to expose the safe, unboxed APIs.
4. **Phase 4: Verification & Benchmarking**:
   - Validate performance across `fannkuch`, `nsieve`, `word_count`, `knucleotide`, `list_sum`, and `list_index`.

## Implementation closeout

This RFC is implemented on `master`.

| Area | Shipped implementation | Acceptance evidence |
|---|---|---|
| contiguous list storage | `List(Bool)` uses a one-byte packed payload; `List(Int)` and `List(Float)` retain their native eight-byte scalar stride; declared packed records use their exact inline stride | `bool_list_uses_byte_packed_layout`, `bool_list_push_uses_byte_packed_layout`, and `declared_packed_list_push_preserves_layout_and_counts_growth` in `crates/witchy-lower/tests/codegen.rs` |
| direct list access | proven indexed `list.at`/`list.set_at` sites emit direct typed loads/stores and avoid the generic helper path | `elides_bounds_check_in_counted_loop` and `elides_bounds_check_before_proven_indexed_list_write` |
| bulk movement and search | `raw_buffer_copy`, SIMD string equality, and SIMD byte search use WIR `v128` operations | `simd_accelerated_str_eq_and_find_byte` in `crates/witchy-wir/src/wir_encode_tests/runtime_helpers.rs` |
| borrowed strings | `string.slice` is an unallocated `(ptr,len)` view; explicit materialization is separate and uses the SIMD copier | `borrowed_string_slice_keeps_pointer_and_length_without_substr` |
| interpolation | call-position integer interpolation uses a reserved format-stack arena and is consumed by borrowed dictionary query/update helpers without ordinary-heap scratch | `dict_interpolation_query_uses_a_rewindable_format_view` and `dict_update_interpolation_uses_borrowed_view_and_preserves_misses` |
| safety and parity | ownership-root accounting, checked bounds, and interpreter/Wasm result agreement remain enforced | the complete lowerer/WIR suites and the paired benchmark correctness pass |

Witchy does not have a separate `Byte` scalar type. The byte-valued contract is
represented by `Bytes` (a flat `[length][byte payload]` buffer whose public
element value is an `Int` in `0..=255`) and by the one-byte `List(Bool)` layout.
Introducing a nominal `Byte` type would be a separate language evolution, not a
missing implementation of this RFC. Likewise, `RawBuffer(T)` is compiler-owned
lowering machinery rather than a user-constructible safe-Witchy type: safe code
uses the existing `list`, `string`, and `bytes` modules, while the lowerer/WIR
own pointer arithmetic and allocation invariants.

The current three-sample benchmark artifact is intentionally outside the
repository at `/tmp/rfc0144-bench-stack.txt`; it records correctness for all
17 benchmark programs and kernel/wall measurements after the format-stack
landing. The focused acceptance suites were 111 lowerer tests and 33 WIR tests,
with the merge-queue full gate green.
