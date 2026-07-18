---
rfc: 0086
title: Capability-gated native extensions and foreign libraries
status: proposed
created: 2026-07-13
superseded-by:
tracking:
---

# RFC-0086: Capability-gated native extensions and foreign libraries

## Summary

Add a typed native-extension boundary for integrating existing C-compatible
libraries and trusted host implementations. Witchy code receives a
`NativeLoader` capability, requests a content-identified module implementing an
expected interface, and calls it through owned values or opaque handles. There
is no ambient `dlopen`, source-level raw pointer type, or unchecked `extern`
declaration.

This is the ecosystem escape hatch needed for database clients, media codecs,
GUI and system libraries, numerical kernels, hardware integrations, and mature
native SDKs. It is also an explicit expansion of the trusted computing base:
capabilities make native use visible and controllable, but cannot sandbox
arbitrary machine code loaded into the host process.

## Motivation

Witchy should be a general-purpose language rather than an island. Its standard
library can cover common work, and [RFC-0085](0085-capability-bounded-dynamic-code.md) can load isolated Witchy modules,
but neither gives an application access to the decades of useful native
libraries that exist behind C ABIs.

The runtime already has a compiled-in registry for pure Rust implementations
such as cryptography and encoding. That registry is intentionally closed and
trusted. Its source notes a future capability-gated dynamic half, but the
language has no decision for package identity, ABI safety, handles, loading,
authority, portability, or failure behavior.

Ruby and Python gain enormous practical reach from native extensions. Their
traditional extension models also let a package execute unrestricted native
code during installation or import, crash the process, violate memory safety,
and use any ambient OS authority. Witchy should gain the reach without hiding
those costs or weakening its package-install and capability contracts.

## Design

### Trust model

Native code loaded into the Witchy host is fully trusted machine code. It can
issue syscalls, corrupt memory, crash the process, or ignore Witchy's logical
grant model. A `NativeLoader` capability controls whether Witchy code may ask
the host to load an extension; it is not a containment boundary for the loaded
library.

Therefore the platform has two extension tiers:

1. Untrusted or least-trust extensions use [RFC-0085](0085-capability-bounded-dynamic-code.md) isolated Witchy/WASM
   modules, a WASM component, or a separate process with a typed message
   boundary.
2. Native extensions are an explicit host trust decision for code whose
   provenance and machine-code behavior the operator accepts.

Documentation, package metadata, `witchy caps`, and launch consent must use the
word "trusted" for the native tier. They must not imply that ordinary capability
confinement contains a malicious native library.

### Typed loading surface

The eventual surface composes with [RFC-0081](0081-existential-trait-values.md) existential interfaces and
[RFC-0085](0085-capability-bounded-dynamic-code.md)'s compile-time `Interface(T)` descriptions:

```witchy
trait ImageCodec:
    fn decode(let self, input: Bytes) -> Result(Image, CodecError)
    fn encode(let self, image: Image) -> Result(Bytes, CodecError)

fn open_codec(loader: NativeLoader) -> Result(NativeModule(dyn ImageCodec), NativeLoadError):
    loader.open(
        module: NativeModuleId("sha256:..."),
        expected: meta.interface(dyn ImageCodec),
    )

fn decode_image(codec: NativeModule(dyn ImageCodec), input: Bytes) -> Result(Image, CodecError):
    codec.decode(input)
```

The host validates the module identity, target, ABI version, export table, and
interface hash before returning a value. `NativeModule(T)` is an explicit
authority-bearing wrapper whose forwarded callable surface is `T`; the inner
value cannot be extracted and stored as a bare existential. Calls are statically
checked against the trait. Loader errors distinguish an unapproved module,
unsupported target, digest mismatch, signature or provenance failure, ABI
mismatch, missing export, and interface mismatch.

Before [RFC-0081](0081-existential-trait-values.md) ships, the first implementation may generalize the existing
native registry behind typed `.witchy` stubs. That phase is host-registered and
statically linked; it does not expose arbitrary path loading or invent a second
untyped dynamic-call API.

### No ambient path loading

Witchy source never passes an arbitrary filesystem path to `dlopen`. A
`NativeModuleId` names immutable content and target metadata. The host resolves
that identity through an operator-approved store. Selecting a library by path,
environment variable, current directory, or platform search path is outside the
language contract.

`NativeModuleId` is sealed and cannot be constructed from a runtime `String`;
the constructor spelling in the example is provisional shorthand for an ID
resolved from locked package metadata or supplied by the host grant. A
configuration-driven choice may select among a finite set of such IDs, in which
case footprint analysis records their union. An unbounded runtime module name is
rejected rather than widened silently to every installed library.

Applications may ask a `Dir` capability to read package metadata, but possession
of `Dir` does not imply permission to execute the bytes it finds. Loading
requires `NativeLoader` separately.

### Package and provenance contract

A rune that offers a native companion declares it in package metadata for each
supported target. The declaration records at least:

- content digest and artifact size;
- target triple and native ABI version;
- expected interface hash and exported module name;
- source/package identity and optional Coven signature;
- whether a portable WASM or pure-Witchy fallback exists;
- declared lifecycle requirements such as threads or process-global state.

Package resolution may fetch and hash these artifacts, but installation does
not execute them. Loading happens only at application runtime after the host's
native-module policy approves the exact identity. Lockfiles pin the identity in
the same way they pin source dependencies.

Native artifacts are published prebuilt or by an explicit trusted build
pipeline. This RFC does not add implicit `configure`, `make`, `build.rs`, or
post-install hooks to the package manager.

### ABI and value boundary

The first ABI supports owned, representation-stable values:

- `Nil`, `Bool`, `Int`, `Float`, and `Duration`;
- copied `String` and `Bytes`;
- recursively owned lists, records, enums, and structured errors whose schema
  is present in the interface description;
- opaque sealed handles owned by one native module.

Witchy does not expose raw pointers, C layout casts, variadic calls, unions,
borrowed stack addresses, or arbitrary callback function pointers. A native
adapter generated from the interface owns all unsafe C ABI conversion and
returns a checked status plus an owned result. Panics and foreign exceptions
must not unwind across the boundary.

Opaque handles carry their module identity and lifecycle state. They cannot be
forged, serialized, reflected into an address, sent to another VM, or consumed
by a different module. Operations on a closed handle return a structured error.
The host guarantees finalization at most once, including during module teardown.

### Authority and footprint

Any call into a third-party native module requires a loader-derived
`NativeModule(T)` value in the function's ordinary inputs. It cannot decay to
`T`, convert to `Dynamic`, or be captured invisibly by another existential. The
program footprint records the exact native module identity, not merely a broad
`Native` category. Launch policy can allow one pinned image codec without
allowing every installed native module.

The typed interface still names logical capabilities explicitly when the host
adapter expects them. This keeps ordinary call sites reviewable and lets benign
adapters follow least authority. It does not constrain hostile native code in
process; the module's trusted status remains part of the grant prompt and audit
record.

A native extension cannot manufacture a Witchy capability value or inspect the
host capability table. Capability-bearing values cross the ABI only through a
host-defined adapter for that exact interface. The default ABI excludes them.

### Portability and backend parity

A native package declares supported targets. Importing it for an unsupported
target is a check or link error with the available fallback named. It never
silently replaces a call with a stub or changes a successful value into a
runtime "unsupported" string.

Portable extensions should provide a pure Witchy or WASM implementation. A
native accelerator that claims equivalence must run differential tests against
that implementation over the supported value domain. Browser builds cannot load
native libraries; they use the declared portable implementation or reject the
package before execution.

Some integrations are inherently host-specific. Those packages may be
target-constrained, but the constraint is visible in package metadata,
documentation, and tooling. The interpreter and compiled backend on the same
supported host call the same registered adapter so backend choice does not alter
semantics.

### Lifetimes, buffers, and callbacks

Version one copies strings, bytes, and aggregates across the boundary. This is
not always fastest, but it is a coherent safe baseline and works in normal mode.

After [RFC-0083](0083-opt-mode-lifetimes.md), `mode opt` may add borrowed buffers whose lifetime is tied to an
opaque native lease. The lease prevents owner mutation, module unload, and
foreign release until all views expire. Borrowed data cannot enter normal owned
APIs without `.owned()`, escape through `Dynamic`, or cross a VM boundary.

Callbacks, reentrant calls into Witchy, foreign threads, and async completion are
deferred. They need explicit scheduler attachment, panic containment, module
liveness, and callback lifetime rules. An adapter cannot smuggle them through an
integer or pointer-shaped field before that design exists.

### Resource limits and failure behavior

The host may bound loaded module count, address-space growth, handle count, call
duration, and concurrent calls. Cooperative cancellation is part of an
interface only when the adapter supports it. In-process native code cannot be
forcibly preempted safely; workloads requiring hard CPU or memory containment
must use an isolated tier.

Ordinary foreign failures return declared `Result` errors. ABI contract
violations, invalid returned layouts, double-finalization attempts, and adapter
panics fail closed as a native-boundary trap with the module identity in the
diagnostic. They are never decoded as valid Witchy values.

### Tooling and verification

`witchy caps` and package documentation show native module identities,
provenance, supported targets, portable fallbacks, and the explicit TCB warning.
`witchy native inspect` can print a module manifest and interface hash without
loading machine code.

The conformance suite includes malformed manifests, digest and interface
mismatches, unsupported targets, invalid adapter outputs, handle lifecycle,
interpreter/compiled parity, and positive controls proving that an ungranted
module cannot load. Sanitizer builds exercise every first-party adapter. Unsafe
adapter code is a proof obligation reviewed independently from Witchy source.

## Staged implementation

1. Generalize the compiled-in pure native registry around typed interface
   metadata and parity tests; no dynamic loading.
2. Add content-identified manifests, lockfile entries, inspection tooling, and a
   host approval policy.
3. Add `NativeLoader` and trusted dynamic modules for owned scalar/aggregate
   values only.
4. Add opaque handles with strict lifecycle tests.
5. Consider [RFC-0083](0083-opt-mode-lifetimes.md) borrowed-buffer adapters and a separate callback RFC after
   the owned ABI is stable.

Each stage is useful without exposing the unsafe surface of later stages.

## Alternatives

- **No native extension mechanism.** This preserves the smallest host but makes
  Witchy impractical for domains whose mature libraries are native.
- **Source-level C declarations and raw pointers.** Familiar and maximally
  flexible, but moves ABI unsafety into every application and conflicts with
  value semantics, backend parity, and capability review.
- **Treat `NativeLoader` as a sandbox.** Rejected because in-process machine code
  can bypass the loader after entry. Naming it a sandbox would be a security bug.
- **Run every extension as WASM.** Preferred for untrusted code and often enough,
  but not all system SDKs, drivers, or performance libraries have a usable WASM
  build.
- **Execute native build scripts during package install.** Common in other
  ecosystems, but violates Witchy's no-install-time-code-execution guarantee.

## Drawbacks

- The native tier can compromise the entire host despite a precise Witchy
  footprint; operator trust is unavoidable.
- A stable adapter ABI and per-target artifact pipeline are substantial product
  commitments.
- Copy-first value conversion costs time and memory until borrowed buffers are
  proven safe.
- Platform-specific packages weaken source portability even when the failure is
  explicit.
- The feature increases supply-chain review and signing pressure around Coven.

## Prior art

Python and Ruby C extensions, CPython's stable ABI, JNI, Node-API, Erlang NIFs,
Rust `unsafe` FFI wrappers, WebAssembly components, browser native messaging,
and out-of-process plugin systems inform this design. Erlang's distinction
between fast NIFs and isolated ports is especially relevant: trusted in-process
speed and enforceable isolation are different products.
