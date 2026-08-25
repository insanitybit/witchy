---
rfc: 0156
title: "Ahead-of-Time Native Compilation for Trusted Executables"
status: rejected
created: 2026-08-25
superseded-by:
related:
  - "0017 (codegen performance)"
  - "0034 (closing the compute gap)"
  - "0092 (trusted application executables)"
  - "0095 (trusted application installation)"
  - "0155 (authenticated executable caches)"
tracking: "Rejected by RFC-0157: its cache-poisoning premise was removed, and AOT work requires measured performance evidence plus a separate compatible-runtime design."
---

# RFC-0156: Ahead-of-Time Native Compilation for Trusted Executables

## Summary

Witchy Trusted Executables (RFC-0092) provide standalone native binaries containing
an embedded WebAssembly module, a checked capability binding plan, and a minimal
Wasmtime runtime launcher. Because RFC-0155 intentionally disables mutable
executable caches for trusted executables to prevent cache-poisoning attacks,
every invocation of a trusted executable currently JIT-compiles its embedded
WebAssembly bytecode via Cranelift in memory at launch time. This incurs 10 to 50 ms
of cold startup latency on every run.

This RFC proposes ahead-of-time (AOT) machine code compilation during the
`witchy build --target trusted-exe` packaging step:

1. **Build-Time AOT Compilation**: The packaging toolchain runs target-specific
   Binaryen optimization and Cranelift compilation at build time for the requested
   host or cross-compilation target triple (for example, `aarch64-apple-darwin` or
   `x86_64-unknown-linux-gnu`).
2. **Authenticated Native Payload Embedding**: The precompiled native machine code
   artifact is embedded alongside the authoritative WebAssembly bytes and binding
   plan inside the executable binary, sealed with a cryptographic recipe digest.
3. **Instantaneous Direct Mapping**: At runtime, the launcher validates the sealed
   descriptor, directly memory-maps the precompiled native code via Wasmtime's
   deserialization engine without invoking the Cranelift compiler, and arms kernel
   confinement before executing the guest entrypoint.
4. **Sub-Millisecond Cold Startup**: Eliminates runtime bytecode validation and JIT
   compilation overhead, bringing trusted executable startup to native speed (< 1 ms)
   without weakening capability isolation.

## Motivation

### The Startup Latency Tax

For command-line utilities, short-lived filters (such as `grep` or `jq` equivalents),
and developer tools, process startup latency dominates total execution time on
small inputs.

Under the current architecture:

```text
Invocation (./my-app)
  -> Kernel execve & runtime init (~0.8 ms)
  -> Parse embedded Wasm & binding plan (~0.4 ms)
  -> Arm OS confinement (Landlock/Seatbelt) (~1.2 ms)
  -> Cranelift JIT Compilation in RAM (15 to 45 ms)  <-- Dominant Bottleneck
  -> Guest Main Execution (< 1 ms for small CLI runs)
```

Because JIT compilation occurs on every process launch, short CLI tasks spend over
90 percent of their wall time compiling code rather than executing user logic.

### Why Runtime Caching Is Insufficient for Trusted Executables

As established in RFC-0155:
- Mutable per-user executable caches (`~/.cache/witchy/`) are outside the trusted
  computing base of a sealed application.
- A guest program granted filesystem-write authority could tamper with or substitute
  cache records used by a subsequent higher-authority program.
- Therefore, trusted executables cannot safely consult unauthenticated mutable
  caches, forcing cold JIT compilation on every launch.

Ahead-of-time compilation at packaging time solves this by placing the compiled
machine code inside the signed, immutable application binary itself.

## Proposed Invariants

1. **Zero Runtime JIT Compilation**: A trusted executable containing a matching
   target AOT artifact must not invoke the Cranelift compiler during startup.
2. **Capability Invariance**: AOT compilation alters only the binary representation
   and launch latency. It must never expand, bypass, or alter capability grants,
   confinement rules, or guest trap semantics.
3. **Cryptographic Binding**: The embedded AOT native artifact, the canonical
   WebAssembly bytecode, and the capability launch contract must share a mutual
   cryptographic digest verified by the launcher before execution.
4. **Safe Fail-Closed Fallback**: If the host CPU microarchitecture does not support
   the CPU features required by the embedded AOT artifact, the launcher must fail
   closed with a descriptive diagnostic rather than executing invalid instructions.
5. **Cross-Compilation Parity**: Packaging for a different target triple must produce
   an exact target-native binary without requiring host execution of the target guest.

## Design

### 1. Build-Time AOT Compilation Pipeline

When invoking `witchy build --target trusted-exe` (or `witchy package`):

```text
Source Code
    |
Checked AST & WIR
    |
Raw WebAssembly Bytecode
    |
Binaryen wasm-opt (-O4 --all-features)
    |
Canonical Optimized WebAssembly (.wasm)
    |
Cranelift AOT Target Compilation (TargetTriple, CpuFeatures, OptLevel::Speed)
    |
Serialized Native Module (.cwasm payload)
    |
Linker / Bundle Assembly -> Final Native Executable
```

The packaging pass uses Wasmtime's Cranelift backend in ahead-of-time mode:

```rust
pub struct AotCompileConfig {
    pub target_triple: String,
    pub cpu_features: Vec<String>,
    pub opt_level: wasmtime::OptLevel,
    pub paged_memory: bool,
}

pub fn compile_aot_module(
    wasm_bytes: &[u8],
    config: &AotCompileConfig,
) -> Result<Vec<u8>, AotError>;
```

### 2. Sealed Binary Payload Format

The executable payload descriptor (RFC-0092) is updated to include an optional
authenticated native segment:

```text
+-------------------------------------------------------------+
| Native Launcher Mach-O / ELF Executable Header & Code       |
+-------------------------------------------------------------+
| Embedded Application Trailer Magic (WITCHY_EXE_V2)         |
+-------------------------------------------------------------+
| Header Length (u32) | Payload Length (u64) | Flags (u32)    |
+-------------------------------------------------------------+
| Launch Contract & Binding Plan (JSON/CBOR)                  |
+-------------------------------------------------------------+
| Canonical WebAssembly Bytecode (Validated .wasm)            |
+-------------------------------------------------------------+
| AOT Native Machine Code Payload (Serialized Cranelift .cwasm)|
+-------------------------------------------------------------+
| Cryptographic Manifest & Recipe Digest (BLAKE3)             |
+-------------------------------------------------------------+
```

The descriptor binds:
- `wasm_sha256`: Digest of the canonical WebAssembly bytecode.
- `aot_sha256`: Digest of the serialized native machine code artifact.
- `target_triple`: The target architecture and OS triple (for example, `aarch64-apple-darwin`).
- `cpu_features`: Supported CPU feature flags (for example, `+neon,+fp16`).
- `binding_digest`: Digest of the declared capability bindings and launch policy.

### 3. Launcher Runtime Execution

When the native launcher starts:

```rust
pub fn run_trusted_application(
    app: &OwnedEmbeddedApplication,
) -> Result<i32, String> {
    // 1. Resolve and verify capability bindings
    let resolved = trusted_exe::resolve_binding_plan(&app.bindings, &app.wasm, &cwd)?;
    
    // 2. Arm OS kernel confinement (Landlock / Seatbelt)
    crate::commands::confinement::arm(&caps, resolved.confinement)?;
    
    // 3. Fast-path: Load precompiled AOT native module if target matches
    let engine = Runtime::aot_engine(&app.aot_target_config)?;
    let module = if let Some(aot_bytes) = &app.aot_native_code {
        unsafe {
            // Safe because bytes are verified against the sealed manifest digest
            Module::deserialize(&engine, aot_bytes)
        }?
    } else {
        // Fallback: JIT compile embedded wasm bytes if AOT payload is absent
        Module::new(&engine, &app.wasm)?
    };
    
    // 4. Instantiate and execute under the sandboxed store
    let mut store = Store::new(&engine, state);
    let mut linker = Linker::new(&engine);
    link_capability_imports(&mut linker, &caps)?;
    
    let instance = linker.instantiate(&mut store, &module)?;
    let entrypoint = instance.get_typed_func::<(), ()>(&mut store, "main")?;
    entrypoint.call(&mut store, ())?;
    
    Ok(0)
}
```

### 4. Performance Expectations

| Stage | Current JIT Trusted Executable | Proposed AOT Trusted Executable |
|---|---|---|
| Process spawn & binary load | ~0.8 ms | ~0.8 ms |
| Confinement setup | ~1.2 ms | ~1.2 ms |
| Module preparation | 15 to 45 ms (JIT Compile) | **0.1 ms (Direct mmap)** |
| Instantiation & Linker | ~0.3 ms | ~0.3 ms |
| **Total Cold Startup Latency** | **17 to 47 ms** | **~2.4 ms** |

For repeated CLI invocations, total startup latency drops by over 85 to 95 percent.

## Security and Safety Considerations

1. **Native Deserialization Safety**:
   `Module::deserialize` in Wasmtime assumes the input bytes are trusted and not
   maliciously crafted. This safety requirement is satisfied because the AOT artifact
   is compiled exclusively during build time, embedded in the sealed executable, and
   authenticated against the sealed cryptographic digest prior to deserialization.
2. **Untrusted User Inputs**:
   Guest programs still run strictly inside Wasmtime's WebAssembly sandbox. Memory
   isolation, linear memory bounds, and capability gating remain enforced by native
   hardware guard pages and capability host import wrappers.
3. **OS-Level Confinement Precedence**:
   Kernel sandbox confinement (Landlock, Seatbelt) is armed before the guest module
   is instantiated and before user entrypoint code runs.

## Alternatives Considered

1. **Standalone Direct C/LLVM Backend**:
   Generating native C or LLVM IR directly from WIR would produce native binaries
   without Wasmtime, but would require rebuilding the entire garbage collector,
   reference type system, stack unwinding, and capability host interface from scratch.
2. **Persistent User Compilation Cache**:
   Relying on a local compilation cache in `~/.cache/witchy/` leaves trusted executables
   vulnerable to local cache poisoning attacks (RFC-0155) and does not benefit the
   first run on a newly installed machine.
3. **In-Process CLI Daemon**:
   Keeping a warm compiler process in the background reduces latency for the `witchy`
   CLI, but does not benefit standalone distributed application binaries.

## Acceptance Criteria

- [ ] `witchy build --target trusted-exe` compiles and embeds target-specific AOT
      machine code into the generated native binary.
- [ ] Cross-compilation for common tier-1 targets (`x86_64-unknown-linux-gnu`,
      `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`)
      generates verified AOT payloads.
- [ ] Cold invocation of an AOT trusted executable achieves sub-3ms total startup
      latency on reference benchmark hardware.
- [ ] Cryptographic digest mismatch or corrupted AOT payloads fail closed cleanly.
- [ ] All capability isolation tests and sandboxing invariants pass without modification.

> 2026-08-25: Rejected after adopting RFC-0157. Cache independence is no
> longer a security requirement, the stated startup numbers were not established
> by matched measurements, and the proposed native deserialization path did not
> preserve the existing Witchy VM execution seam. AOT may be proposed again on
> performance evidence without reviving the rejected cache-writer threat model.
