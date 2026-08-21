---
rfc: 0140
title: "WebAssembly 128-bit SIMD and Relaxed SIMD runtime acceleration"
status: proposed
created: 2026-08-21
related:
  - "0017 (codegen performance constant factors)"
  - "0029 (performance tier contract)"
  - "0031 (simd stdlib hot loops)"
  - "0034 (closing the compute gap - codegen and runtime levers)"
  - "0134 (codegen and compute optimization roadmap)"
  - "0139 (high-ROI compute, heterogeneous dictionary lookup, and async channel optimizations)"
tracking: "Proposes enabling wasm_simd and wasm_relaxed_simd for internal runtime routines: SwissTable 16-way group probing, vectorized string scanning, and bulk XXH3 hashing"
---

# RFC-0140: WebAssembly 128-bit SIMD and Relaxed SIMD runtime acceleration

## Summary

WebAssembly 128-bit Fixed-Width SIMD (`v128`) has been a finalized W3C standard since 2021 and is enabled by default across all modern engines (Chrome 91+, Safari 16.4+, Firefox 89+, Node.js 16+, Wasmtime, Wasmer). Furthermore, `relaxed-simd` is universally supported across production engines.

While prior designs deferred SIMD due to concerns regarding cross-architecture floating-point determinism (e.g., FMA differences between x86 and ARM), **runtime-internal SIMD acceleration for collections, hashing, and string operations carries zero user-observable non-determinism**. Differential testing in Witchy runs locally on the host, comparing the AST interpreter and Wasm JIT on the exact same hardware.

This RFC proposes enabling `wasm_simd` and `wasm_relaxed_simd` in the runtime engine and introducing vector operations into WIR to accelerate three critical performance bottlenecks:
1. **SwissTable 16-Way Parallel Metadata Probing (`$dict_find`)**: Replacing serial open-addressing with 16-slot parallel control-byte matching (`i8x16.eq` + `i8x16.bitmask`).
2. **Vectorized Substring & Byte Scanning (`string.index_of`, `bytes.index_of`)**: Scanning 16 bytes per cycle rather than byte-by-byte loops.
3. **High-Throughput Vectorized Hashing (XXH3)**: Parallel 64-byte block hashing using `i64x2.extmul_low_i32x4_u` vector multipliers.
4. **Relaxed Swizzles (`i8x16.relaxed_swizzle`)**: Eliminating out-of-range bounds-zeroing emulation overhead on x86 (`PSHUFB`) and ARM (`VTBL1`).

---

## Motivation & Architecture Scope

### 1. Differential Testing Is Host-Local

Witchy's differential testing engine (`link_run` vs `wasm_run`) executes both backends on the *same machine*. Because both the reference interpreter and the compiled WebAssembly run on the identical CPU architecture, host-level instruction variances do not cause differential failures.

### 2. Encapsulated Runtime Semantics

SIMD instructions are strictly internal to standard library operations and compiler-generated runtime helpers:
* `Dict` lookups only expose key/value pairs; the internal bucket layout and probe sequence are completely opaque to user code.
* String search (`string.index_of`, `string.contains`) produces exact, deterministic integer indices.
* Digest and hash helpers produce identical results regardless of vector register widths.

User code does not directly manipulate unconstrained floating-point vector registers, ensuring full semantic stability across platforms.

---

## Technical Specification

### 1. Runtime Engine Configuration

In `crates/witchy-runtime/src/runtime.rs`, enable SIMD capabilities in the Wasmtime engine configuration:

```rust
// crates/witchy-runtime/src/runtime.rs
config.wasm_simd(true);
config.wasm_relaxed_simd(true);
```

### 2. WIR Extensions (`crates/witchy-wir`)

Introduce vector types and instructions into WIR:

```rust
// crates/witchy-wir/src/wir.rs

pub enum WirTy {
    Int,
    Bool,
    Float,
    V128,  // 128-bit vector register
    // ...
}

pub enum VectorOp {
    // Splat / Broadcast
    I8x16Splat,
    I32x4Splat,
    
    // Comparisons & Masks
    I8x16Eq,
    I8x16Bitmask,
    
    // Bitwise & Selection
    V128And,
    V128Or,
    V128Xor,
    V128Bitselect,
    
    // Arithmetic & Widening Multiplication
    I64x2ExtMulLowI32x4U,
    I64x2ExtMulHighI32x4U,
    
    // Relaxed Operations
    I8x16RelaxedSwizzle,
}
```

---

## Accelerated Subsystems

### 1. SwissTable 16-Way Parallel Probing (`$dict_find`)

#### Current Architecture (Linear / Quadratic Probe)
Currently, `$dict_find` performs open-addressing where each probe requires a full key comparison or scalar hash check. On collisions or sparse tables, multiple cache lines and branches are traversed serially.

#### Vectorized SwissTable Design
The hash table maintains a metadata array of 1-byte control tags for every slot:
* `0x80`: Empty slot
* `0xFE`: Deleted slot (tombstone)
* `0x00..=0x7F`: 7-bit hash fingerprint ($H_2 = \text{hash} \ \& \ \text{0x7F}$)

A lookup executes:
1. Compute full hash $H = \text{hash}(k)$. Split into $H_1$ (group index) and $H_2$ (7-bit fingerprint).
2. Load 16 control bytes at group offset $H_1$: `v128.load(ctrl_ptr + H1)`.
3. Broadcast $H_2$: `let needle = i8x16.splat(H2)`.
4. Match tags: `let matches = i8x16.eq(group, needle)`.
5. Extract bitmask: `let mask = i8x16.bitmask(matches)`.
6. Traverse only the matching bits using `i32.ctz` (count trailing zeros).

```wasm
;; 16-slot group match in 4 instructions
v128.load offset=0 (local.get $group_ptr)
i8x16.splat (local.get $h2)
i8x16.eq
i8x16.bitmask
local.set $match_mask
```

**Performance Gain**: Checks 16 slots in 3 Wasm instructions with zero branch mispredictions on empty slots.

---

### 2. Vectorized String & Byte Scanning

String searching (`string.index_of`, `bytes.index_of`) scans 16-byte blocks in parallel:

```wasm
;; Loop over 16-byte chunks
loop $scan
  v128.load (local.get $ptr)
  local.get $target_splat
  i8x16.eq
  i8x16.bitmask
  local.tee $mask
  if
    ;; Match found: calculate exact index with ctz
    local.get $ptr
    local.get $mask
    i32.ctz
    i32.add
    return
  end
  local.get $ptr
  i32.const 16
  i32.add
  local.set $ptr
  br $scan
end
```

**Performance Gain**: 4–8x speedup on string searching and delimiter splitting over scalar byte loops.

---

### 3. Bulk XXH3 Hashing with 128-bit Vector Multipliers

For large strings and byte arrays (> 32 bytes), XXH3 processes 64-byte stripes per iteration. Using `i64x2.extmul_low_i32x4_u` and `i64x2.extmul_high_i32x4_u`:
* 4 parallel 64-bit accumulators are updated per vector instruction.
* Throughput scales from ~1.2 GB/s (scalar word loop) to **6–8 GB/s** (SIMD128 loop).

---

## Benchmark Impact Projections

| Benchmark / Operation | Current Baseline | Projected with SIMD / SwissTable | Estimated Speedup |
| :--- | :--- | :--- | :--- |
| **`dict_find` (16-slot probe)** | 12–25 cycles | 3–4 cycles | **3.5x–5.0x** |
| **`word_count` (dict upserts + hashing)** | 139.8 ms | ~45.0 ms | **3.1x** (surpasses Go's 51.0 ms) |
| **`knucleotide` (k-mer table lookups)** | 53.5 ms | ~20.0 ms | **2.6x** (matches Go's 17.2 ms) |
| **`string.index_of` / `bytes.find`** | 1.1 GB/s | 6.5 GB/s | **5.9x** |
| **Bulk byte hash (1 KB payload)** | 850 MB/s | 5.8 GB/s | **6.8x** |

---

## Implementation Plan

### Stage 1: Runtime & WIR Infrastructure
1. Update `witchy-runtime` engine settings to enable `wasm_simd(true)` and `wasm_relaxed_simd(true)`.
2. Add `WirTy::V128`, `Kind::V128`, and vector instruction variants to `crates/witchy-wir`.
3. Add binary encoding support for Wasm vector opcodes (`0xFD` prefix) in `witchy-wir` encoder.

### Stage 2: Vectorized String Operations
1. Implement 16-byte chunk scanning in `string_index_of_helper` and `bytes_find_helper`.
2. Verify parity against existing scalar fallbacks in `src/example_tests`.

### Stage 3: SwissTable Dictionary Probing
1. Implement `$dict_find_simd` and `$dict_insert_simd` in `crates/witchy-wir/src/wir_helpers/dict/lookup.rs`.
2. Connect dictionary key hashing to 7-bit $H_2$ tag extraction and 16-way bitmask matching.
3. Validate performance across `dict_count`, `word_count`, and `knucleotide` benchmarks.
