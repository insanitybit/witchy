---
rfc: 0085
title: Capability-bounded dynamic code compilation and loading
status: proposed
created: 2026-07-12
superseded-by:
tracking:
---

# RFC-0085: Capability-bounded dynamic code compilation and loading

## Summary

Add explicit capabilities for compiling and loading witchy code at runtime. A
loader receives source or a verified package, an expected typed interface, and an
authority ceiling. The resulting isolated module can use no authority beyond
that ceiling, even if its source requests more. There is no ambient `eval` in the
language.

This supports REPLs, plugin systems, user automation, notebook-like tools, and
dynamic application configuration while preserving Witchy's central claim that
authority remains granted, auditable, and enforceable.

## Motivation

RFC-0080 supplies deterministic compile-time generation. RFC-0081 and RFC-0082
provide runtime polymorphism once code is linked. General-purpose dynamic systems
also need to load implementations that were not part of the original compilation:

- editor and build-tool plugins;
- server extensions installed without rebuilding the host;
- interactive shells and notebooks;
- user-authored policy and automation;
- hot-loaded application modules.

An unrestricted `eval(String)` would bypass reproducible builds, package
footprints, tooling, and deployment review. Prohibiting dynamic code entirely
would make Witchy's dynamism stop at a closed-world boundary. The capability
model gives a principled middle path.

## Design

### Capabilities and entry points

Two host capabilities are distinct:

```witchy
Compiler       # compile source to a validated witchy module
ModuleLoader   # instantiate a validated module under an explicit grant
```

Hosts may grant one without the other. A production plugin host can load signed,
precompiled modules without accepting source compilation; a local REPL can hold
both.

The standard API is shaped as:

```witchy
type AuthorityCeiling
type CompiledModule
type Loaded(T)

compiler.compile(c: Compiler, source: String,
                 expected: Interface(T)) -> Result(CompiledModule, CompileError)

loader.load(l: ModuleLoader, module: CompiledModule,
            grants: Grant, ceiling: AuthorityCeiling,
            expected: Interface(T)) -> Result(Loaded(T), LoadError)
```

`Interface(T)` is produced from an RFC-0081 existential trait or another closed
typed export description through `meta.interface(T)`, a compile-time-resolved
type-position operation analogous to RFC-0082's `meta.runtime_type(T)`. Types do
not become ordinary runtime values. Loading a bag of untyped global names is not
the base API.

### Authority ceiling

The loader computes the module's runtime and build footprints using the same
compiler analysis as package publication. Loading succeeds only when:

1. the requested footprint is a subset of the caller-provided ceiling;
2. the concrete grant is a subset of that ceiling;
3. every requested host import is present in the concrete grant;
4. the exported interface matches `Interface(T)`;
5. capability-bearing values do not cross the module boundary outside that
   interface.

The ceiling is monotone and cannot be widened by loaded code. The Wasmtime linker
still omits every ungranted import, so analysis and runtime enforcement are
independent layers.

Passing data to loaded code grants no authority. Passing a capability is allowed
only when the expected interface names its type and rights. RFC-0082 `Dynamic`
cannot carry capabilities and therefore cannot smuggle one through an untyped
argument.

### Isolation and lifecycle

Loaded code runs in a separate WASM instance by default, with its own linear
memory, resource limits, epoch budget, capability tables, and deterministic
message boundary. Calls copy or serialize ordinary values according to a
versioned component ABI. RFC-0081 witness adapters expose the loaded interface to
the host.

An explicit future optimization may co-locate trusted modules, but co-location
is not observable and cannot weaken the grant. The default remains isolation.

`Loaded(T)` owns the instance lifecycle. Dropping it closes resources and
invalidates outstanding calls. Borrowed values cannot escape the instance unless
materialized; RFC-0083 lifetimes describe synchronous borrowed call results only
after a separate ABI design proves them safe.

### Provenance, reproducibility, and policy

A `CompiledModule` records source hash, compiler version, expanded-source hash,
footprints, export interface hash, and optional package signature. Hosts may
require signed Coven provenance, a lockfile identity, or exact compiler versions.

Runtime compilation is intentionally reproducible: compile-time code remains
zero-capability and deterministic. Any requested build step is a separate
RFC-0068 execution under build capabilities and cannot occur implicitly inside
`compiler.compile`.

### Interactive convenience

A REPL may provide:

```witchy
repl.eval(source) -> Result(Dynamic, EvalError)
```

but this is a library over `Compiler`, `ModuleLoader`, an explicit expected
dynamic-result interface, and a visible ceiling. Calling it without those
capabilities is a type error; changing its ceiling changes the host program's
footprint.

### Caching and denial of service

Compilation and loading consume explicit CPU, memory, module-size, expansion,
and wall-time budgets. Cache keys include all provenance fields and engine
configuration. Cached artifacts re-enter Wasmtime through safe validation; no
application-owned native artifact is deserialized.

## Alternatives

- **No dynamic code.** Smallest trusted surface, but rules out important plugin
  and interactive workloads.
- **Ambient `eval`.** Flexible but irreconcilable with auditable authority and
  reproducible package behavior.
- **Native shared-library plugins.** Mature and fast, but escape the WASM sandbox
  and make native ABI safety part of every plugin's trust contract.
- **Recompile the whole application.** Appropriate for static deployment, not
  interactive or user-extensible systems.

## Drawbacks

- Runtime compiler access materially expands the host's attack surface.
- Cross-instance calls and value transfer have real overhead.
- ABI, compiler-version, and metadata compatibility become product commitments.
- Conservative authority ceilings may require hosts to grant more categories
  than one selected implementation eventually uses.

## Prior art

WebAssembly components, Erlang code loading, JVM class loaders, Lua embedding,
browser workers, capability-safe plugin hosts, and Nix-style content identity
inform this design.
