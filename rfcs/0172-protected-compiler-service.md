---
rfc: 0172
title: Protected compiler service with warm state and shared compilation
status: proposed
created: 2026-09-26
related: [0157, 0163, 0165, 0167, 0168, 0169]
amends: "0157: cache-integrity policy for service-managed state only"
tracking: "Proposal only. Phased acceptance ledger below; no implementation or measured speedup claimed."
---

# RFC-0172: Protected compiler service

## Summary

Provide a local, long-lived compiler service under its own unprivileged OS
identity. It owns trusted compilation caches, retains reusable compiler state
in memory, and combines concurrent requests for identical work into one job.
The command-line driver remains responsible for projects, dependency resolution,
build-step orchestration, output files, and running programs. The service uses
the existing canonical compiler and SafeWIR admission path; it is not another
compiler implementation. Start with Linux, whole-request reuse, bounded warm
state, and in-flight deduplication. Fine-grained incremental compilation and
native artifact delivery are later, independently gated extensions.

Standalone `witchyc` remains a first-class, fully supported compiler, not a
fallback implementation. One-shot compilation, the protected Witchy service,
and an optional Bazel-managed worker host the same compiler library. Neither
correctness nor availability of compiler features depends on a daemon or cache.

## Motivation and current baseline

A separate process alone does not make compilation faster. Avoiding repeated
work does: loading compiler state, reconstructing identical modules, checking
the same SafeWIR, and concurrently compiling the same inputs. Keeping this state
under the developer's identity also leaves its integrity exposed to code that
can modify that identity's files.

At `e48a91059`, the relevant existing mechanisms are:

- `src/commands/compile.rs::compile_checked_to_wasm_cached` deliberately
  bypasses detached production caching: raw cached bytes do not preserve
  admitted-artifact authority.
- `crates/witchy-wir/src/canonical_safewir/check/cache.rs` already has bounded
  process-local verdict memoization, an opt-in persistent verdict store, and
  cross-process single-flight locking for large checks. This RFC extends reuse
  across compilation stages; it does not claim deduplication is entirely new.
- `crates/witchy-runtime/src/runtime.rs` configures Wasmtime's native compilation
  cache. Entering through `Module::new` does not make the engine's underlying
  native cache independent of its writers' trustworthiness.
- [RFC-0169](0169-compile-std-once.md) proposes reusable standard-library
  artifacts. Those provide a reusable compilation unit; this service provides
  the lifetime, ownership, storage, and scheduling around such units.

These are implementation observations, not timing measurements. The expected
benefits are shorter repeated checks/builds/runs and less duplicated CPU under
concurrency. Cold compilation and guest execution are not promised to improve.

## Scope and contract relationships

- **RFC-0167:** no source syntax, ownership, effect, normal/opt, or resource
  semantics change. Caching must preserve the existing admission boundary and
  unsupported forms still fail closed. No language acceptance row closes here.
- **RFC-0168:** preserve the compiler/driver split. The service wraps the same
  explicit-input compiler library as `witchyc`; it does not resolve packages,
  access registries, or move Wasmtime into the compiler core. The service can
  precede the complete packaging split. Later native compilation lives in an
  adapter above the compiler core.
- **RFC-0169:** do not implement a competing std artifact or assume arbitrary
  whole-program typed state is reusable. Consume that artifact when available.
- **RFC-0157:** propose a scoped change to its Summary, Trust model's cache
  exclusion, Ordinary executable caching, and Native artifacts policy. Ordinary
  user-level cache writers become in-scope attackers against service-managed
  state. Ownership isolation, not credentials or hidden capability exclusions,
  supplies the new boundary. Publisher trust and the consequences of writing
  other executable state remain unchanged. Its native safety and measurement
  obligations still apply. This proposal does not revive RFC-0155 or RFC-0156
  wholesale, and does not change the implemented policy until adopted and built.

Non-goals: operating our own remote/distributed build service, a registry, an
execution daemon, shared guest heaps, cross-user source deduplication, new
optimizer passes, or making arbitrary broad filesystem writes harmless. Windows is lowest priority;
macOS is a later adapter. Linux-first is the working platform assumption from
the discussion, which mentioned Linux for both present and future priorities.
Running standalone `witchyc` inside an external remote build executor is in
scope for the compiler interface; it does not require our service on that host.

## Design

### 0. Standalone compiler, optional hosts

The compiler library owns semantic compilation and admission. A host supplies
explicit inputs, a compilation session, optional cache access, cancellation,
and resource budgets. Host-specific IPC, process management, installation, and
cache ownership stay outside the compiler core. Sessions must support repeated
requests without requiring a service connection or hidden process-global state.

| Host | Lifetime and scheduling | Reuse and trust |
|---|---|---|
| One-shot `witchyc` | One invocation, owned by the caller | No persistent state required; performs all checks locally |
| Witchy service | Service-managed bounded workers and single-flight jobs | Protected cache ownership and warm sessions |
| Bazel worker adapter | Bazel owns worker lifetime and action scheduling | Warm sessions within the build environment's trust boundary; no Witchy daemon required |

`witchyc` executes locally by default and never implicitly contacts or starts
the Witchy service. The project driver's service selection is a separate policy.
The standalone interface follows RFC-0168: explicit source/dependency inputs,
toolchain/std inputs or an identified bundled std, target/options, output paths,
diagnostics, and exit status. It performs no implicit dependency download or
project discovery and requires neither service installation nor administrator
rights, a populated home directory, or a writable global cache. Build-time
effects require explicit inputs/authority; a hermetic build cannot acquire
undeclared environment or filesystem dependencies through the compiler.

For identical logical inputs and options, all hosts preserve the same language
semantics, diagnostics, admission obligations, and output contract. Cache state
may change cost, not meaning. Disabling caches must not disable compiler
features. Service-specific isolation limits can reject a request, but cannot
define a different language or weaken the standalone admission path.

A future thin Bazel adapter translates its worker request/response protocol
into this session API. The compiler also remains runnable as an ordinary Bazel
action with declared inputs and outputs. Bazel owns its action cache and
scheduling; the adapter must not secretly route requests through our service or
create a competing machine-wide worker pool. Initial worker support may process
requests serially; multiplexing is optional and requires request-state isolation.
The adapter scopes each request's filesystem access, options, diagnostics, and
cancellation to that request. Retained state uses the same dependency keys and
memory bounds as other hosts, never the previous action's incidental files.

A Bazel worker is not automatically a separately owned trusted-cache service.
It relies on the build environment's isolation and cache provenance policy.
Externally cached source/Wasm remains subject to the relevant load/admission
checks; a generic action-cache hit does not authorize replaying a SafeWIR verdict
or unsafe native deserialization. No-daemon support removes the need for shared
persistent trusted state; it does not claim equivalent protection for a
same-user writable cache. The protected service's IPC protocol need not
be the compiler library API or Bazel's wire protocol.

### 1. Components and ownership

The following is the optional protected-service deployment, not a required
route for one-shot compilation or a Bazel-managed worker:

```text
driver (user identity)
  | explicit immutable inputs, requested operation
  v
compiler service (dedicated identity)
  | request admission + cache + single-flight scheduler
  v
bounded warm compiler workers -> canonical SafeWIR admission -> artifacts
  |
  +-- result over authenticated local IPC --> driver / ordinary runtime host

build steps and guest execution: separate workers, never cache-owning identity
```

The service and its compiler workers are trusted toolchain code. Process
separation for crash recovery does not make a compiler worker untrusted or
prove its output correct. Initially workers may share the service identity;
source code, plugins, build scripts, and guest programs must never execute as
native code with that identity. Compiler evaluation of user code must use a
bounded, confined evaluator without cache authority, not a service-owned shell.

The service binary, installed toolchains, configuration, endpoint directory,
cache directories, and their controlling ancestors must not be writable by
ordinary clients. Service workers are non-root. Installation and upgrades can
require administrator authority, but compilation does not.

Linux's initial adapter uses a system service with a dedicated account, an
authenticated Unix-domain socket in a protected runtime directory, and a private
cache directory. systemd can manage directory ownership and resource limits;
the domain interfaces do not depend on systemd. Client and server verify peer
identity; endpoint spelling or a client-supplied UID is not authentication.
The service needs neither network access nor blanket access to user home trees.

### 2. Explicit requests and immutable inputs

The logical protocol, independently of its eventual wire encoding, contains:

- a protocol version and service-selected installed toolchain identity;
- an operation (`check` or compile with explicit output kinds);
- a source/dependency manifest with logical paths, module/package identities,
  authenticated ownership metadata where required, and supplied content;
- compiler options, language modes, target/ABI and relevant host-contract
  configuration;
- a request ID for cancellation and diagnostics, distinct from the cache key.

The driver reads inputs using its own authority. The service copies/uploads
them into immutable snapshots and computes their digests. A digest is a lookup
hint, not proof of possession, ownership, package provenance, or permission to
read another user's bytes. Client-supplied owner labels cannot mint the loader
authority currently required by the compiler. The IPC boundary must preserve
the RFC-0163 loader checks, not serialize a `CheckedModule` and trust its name.

The same captured bytes must be hashed and compiled. An open read-only file
descriptor is not necessarily immutable if another process can write the file.
Do not hash a path and later reopen it to compile. Bind diagnostics to the
captured source versions so edits while a request runs cannot mislabel results.

The service returns artifacts and diagnostics; the driver writes output paths.
There is no arbitrary filesystem read/write API, upload-a-verdict API, or
client-selected compiler executable/plugin path. Toolchain selection chooses
only administrator-installed, verified generations. Developer-built toolchains
use the standalone/untrusted development path, not the protected service's
native artifact authority.

Build steps remain outside the service. Generated source can be submitted as
ordinary untrusted input. Reuse or deduplication of effectful build/comptime
evaluation is not automatically authorized: either its observations are fully
captured under the existing effect contract, or that work is not shared.
Ambient environment values, clocks, randomness, network responses, filesystem
observations, and effectful callbacks cannot disappear behind a cache hit.
Inputs whose required evaluation isolation is not implemented are explicitly
unsupported by service mode; they are not evaluated with service authority.

### 3. One generic cache and work-identity model

Use immutable artifacts and typed stage keys rather than cases for individual
language types or hard-coded programs. Conceptually:

```text
key = digest(stage/schema, toolchain identity, input artifact identities,
             semantic options, target/ABI, relevant authority context)
```

Keys include the full transitive dependency set read by that stage. Compiler
identity covers every component affecting the result, the embedded std, checker,
optimizer, and any native backend configuration where relevant. Source text
alone is insufficient: package identity, layouts, host contracts, callable
contracts, mode provenance, and options can change meaning without changing it.
Canonical serialization replaces debug formatting, pointer values, timestamps,
or process-local numeric IDs as a cross-session identity scheme.

Begin conservatively with a complete request key. Narrower stage keys are added
only when their input dependency boundary is explicit. Unknown dependencies
cause recomputation or rejection, not an optimistic hit. Request-specific
capability grants and runtime resource bindings are never reused as compilation
authority; the runtime checks them for each execution.

Cache tiers are:

| Tier | Reusable state | Initial policy |
|---|---|---|
| Warm memory | Immutable input blobs, completed admitted artifacts, existing valid checker memos | Required in initial service |
| Warm compiler session | Parsed modules, std state, typed/lowered stage products with explicit dependency keys | Add incrementally; no new std format |
| Protected disk | Versioned complete artifacts and optional expensive stage results | Survives service restart; never import old user-writable verdict/native caches |
| Native artifact cache | Wasmtime output with exact engine/target provenance | Later phase with separate loader acceptance |

An arbitrary typed graph cannot be shared just because it is in memory. Reuse
must retain its owning immutable arena/context, or remap identities through a
defined import boundary. Mutable inference tables and request-local allocators
must not leak between jobs. Start with a coarse session per compatible context;
do not make serialized arbitrary AST/type graphs a prerequisite for useful reuse.

Every tier is bounded by bytes, not only entry count. Account for retained arenas,
worker heaps, pending input uploads, active artifacts, and waiters. Evict idle
entries and recycle workers when needed. Memory pressure may lose performance,
never change compilation results. Do not share Wasmtime stores, instances,
memories, host handles, or capability bindings across guest runs.

### 4. Deduplicate work without serializing compilation

For each authorized key, the scheduler implements single-flight:

```text
absent -> one queued/running producer + attached waiters -> immutable result
```

Authenticate and apply admission quotas before joining work. One caller's
cancellation detaches only that caller; it cannot cancel other waiters' job.
When no waiters remain, the service may cancel the producer. Disconnects,
timeouts, worker crashes, and shutdown remove in-flight entries and notify
waiters; failed/partial work cannot become a successful cached result. Initial
negative caching is limited to delivering a deterministic error to current
waiters, not persisting transient failures.

Different keys run in a bounded worker pool with per-user fairness and a global
CPU/memory budget. A single cache mutex must not cover compilation. Recursive
stage requests form a dependency DAG; detect cycles rather than letting a job
wait for itself. Completed stages may be published atomically and reused after
a later stage fails if they independently satisfy their contracts.

Private inputs, diagnostics, and artifacts are partitioned by authenticated user
and toolchain/context. The first implementation has no cross-user content lookup
or single-flight joining, even for a guessed digest. Public installed std inputs
may be shared explicitly. Sessions, artifact handles, and status queries cannot
be used to read another user's source or results.

### 5. Preserve admission and artifact provenance

A cache hit reuses a previously established fact; it does not weaken SafeWIR
admission or skip the module-level obligations not covered by a function memo.
Protected storage establishes who produced a record, not that its key captures
all semantic dependencies. Both provenance and complete identity are required.

Persistent publication is atomic and uses bounded, versioned decoding with
length/digest checks. Invalid entries are misses or explicit errors. Protect
against symlink/path traversal and partially published files. Never promote
existing user-writable caches to trusted storage just by moving them.

For admitted Wasm, bind the returned bytes, entry/launch contract, compiler
generation, admission schema, and request key into the response from the
authenticated service. Crossing IPC must preserve that binding in the receiving
host. Bytes saved by the user and loaded later are ordinary untrusted artifacts
unless re-established through the admission path; an unkeyed digest is not an
admission credential. A disk cache must serialize enough authority-bearing
metadata to reconstruct this boundary, not just a success bit.

Native caching is not required for the initial release. Until its delivery path
is implemented, service-mode runs compile admitted Wasm using a runtime with
user-writable persistent native caches disabled. Otherwise the compiler service
would protect one cache while execution still trusts another writable cache.

Later, a native adapter can precompile admitted Wasm and deliver its exact output
through authenticated IPC into privately owned receiver memory or a protected,
immutable handle. Bind engine version/configuration, CPU features, target,
admitted Wasm digest, and launch metadata. Never validate a digest and then
reopen a mutable native-cache path. Unsafe deserialization is permitted only
when provenance and lifetime satisfy the pinned Wasmtime API's contract.
Checksums and SafeWIR verification cannot validate arbitrary native bytes.

Execution stays in the ordinary runtime host with fresh stores, grants, limits,
cleanup, traps, and exit handling. No guest process inherits cache descriptors
or the service identity. Protection of the cache does not prevent a native
attacker from modifying its own user-owned runtime executable or process.

### 6. Lifecycle and fallback

Use versioned endpoints/handshakes and installed toolchain generations. An upgrade
drains or terminates old jobs explicitly; it never silently executes a request
with another compiler version. Cache migration is a miss unless compatibility
is established. Request-scoped compiler flags do not mutate global process
environment or process-global configuration read by another session.

Proposed project-driver policy is `--compiler-service=auto|required|off`.
This does not change standalone `witchyc`'s local-only default:

- `auto`: use a compatible protected service; if absent/unavailable, report the
  fallback in diagnostic/status output and compile locally without trusting
  persistent user-writable verdict/native caches.
- `required`: fail if a compatible authenticated service is unavailable.
- `off`: deliberately select the first-class standalone compiler with that safe
  cache policy. Do not probe for, connect to, or start a service.

An authentication failure is an error, not a silent downgrade. Fallback may
recompute work but must not repeat effectful build steps already performed.
No same-user background daemon is advertised as equivalent ownership isolation.
An explicit developer-only trusted-cache mode, if retained, remains outside the
protected guarantee and cannot feed protected service results.

Expose service generation, cache bytes/hits/misses/evictions, actual producer
counts, deduplicated waiters, queue time, per-stage CPU, and cancellation/crash
counts. Normal logs omit source, secrets, and full private paths. Status output
is scoped to the requesting user; administrator controls own global quotas.

## Delivery and acceptance ledger

Implement small vertical slices on the existing compiler. Do not wait for full
RFC-0167 completion, fine-grained incrementality, or every platform adapter.
Security boundaries apply from the first slice. Focused smoke and adversarial
boundary tests accompany each slice; exhaustive language parity stays in the
compiler's later verification phase rather than becoming service scaffolding.

| Phase | Deliverable and acceptance | State |
|---|---|---|
| A | Host-independent session/request/result API and first-class one-shot compiler; representative check/build results agree with service execution; standalone works with no service, empty home, no network and no persistent caches, and required loader authority survives the boundary | Proposed |
| B | Linux protected installation, authenticated IPC, bounded warm result cache and whole-request single-flight; a cold concurrent identical-request test observes one producer, while different requests can progress concurrently | Proposed |
| C | Protected persistence plus bounded reusable compiler contexts; demonstrate warm reuse of an existing checker/std/frontend stage without request-local identity leakage; restart preserves lawful disk hits | Proposed |
| D | CLI integration and fallback, quotas, cancellation, crash recovery, upgrade behavior, and tests rejecting cache writes/spoofed peers/cross-user reads/native uploads by ordinary clients | Proposed |
| E | Native artifact adapter, immutable delivery and runtime consumption with preserved launch behavior; matched startup evidence and explicit unsafe-loader review | Deferred extension |
| F | Fine-grained dependency invalidation, incremental IDE sessions, and macOS/Windows adapters using the same contract | Deferred extension |
| G | Thin Bazel worker adapter, independent of B-F; ordinary sandboxed one-shot action and persistent worker agree, A/B/A request sequences show no stale state, and cancellation/worker restart preserve results without contacting a Witchy service | Optional integration |

A-D define the initial service. B must already enforce the identity, quotas,
and cancellation invariants; D completes the integration and evidence, not a
temporary authorization bypass. File ownership and shared API contracts must be
recorded before parallel implementation. Freeze A's contract first; installer,
scheduler, storage, and driver work can then proceed independently. Standalone
acceptance in A is mandatory from the first slice; the Bazel-specific adapter
in G can follow independently once A exists and does not block service delivery.

### Performance evidence

Use the workload spread in `scripts/compile-bench.sh`, not just the async
executor. Its current best-of CPU measurement with verdict caching disabled is
not a daemon benchmark: extend the harness to include service CPU, IPC, queue
time, wall-time distributions, retained/peak memory, and actual producer counts.
Pin compiler builds, inputs, host configuration, and cache states; keep raw
samples. Compare standalone and service on matched cache policies, separately
from any security-migration cost of disabling an old native cache.

Required scenarios are cold service/cold disk, restarted service/warm disk,
warm-memory repeat, small edit, unrelated edit, simultaneous identical requests,
different concurrent requests, and capacity pressure. Include simple programs,
typechecking-heavy code, and async/std-heavy code. Report client and service CPU
together; moving CPU into a daemon is not reducing CPU.

Acceptance requires measured benefit for repeated work and elimination of
duplicate producers for identical admitted requests. Report cold-path and memory
regressions candidly; select budgets/defaults from the measurements rather than
inventing a speedup target. A service does not make a slow cold compiler fast.
Small tests must also vary dependencies, owner identity, modes, host contracts,
and toolchain versions to demonstrate invalidation. A recompute-and-compare
mode diagnoses key mistakes without forcing every production hit to recompute.

## Alternatives and drawbacks

- **Only protect a disk cache:** useful and simpler, but loses warm compiler
  structures and centralized in-flight coordination. Keep this as the baseline
  when measuring whether resident state pays for its memory.
- **Same-user daemon:** can provide performance benefits, but not the proposed
  filesystem ownership boundary against ordinary user-level native writes.
- **Fine-grained incremental compiler first:** potentially valuable, but delays
  whole-request reuse and deduplication that need no new dependency engine.
- **A remote build service:** expands authentication, confidentiality, transport,
  availability, and deployment scope. Not needed for local reuse.

Costs include installation/update machinery, an additional trusted IPC parser,
persistent memory, cache invalidation complexity, contention/fairness, and a
larger impact from a service crash or compiler compromise. Whole-request caching
helps unchanged requests but not every edit. Fine-grained reuse needs compiler
architecture work and does not arrive automatically with a daemon. Cache
isolation is not a defense against root/admin, a compromised trusted toolchain,
or all persistent effects of a program's granted filesystem writes.

## Prior art

See [compiler service and native-cache notes](../external-refs/compiler-service-2026/notes.md)
for primary sources. Roslyn's bounded metadata-symbol cache is a precedent for
warm compiler state; Kotlin and Bazel workers show optional long-lived compiler
hosts. BuildBuddy action merging informs in-flight sharing and cancellation.
Nix multi-user mode separates store ownership from untrusted build identities.
These are complementary patterns, not an exact implementation to copy. Roslyn's
compiler-JIT startup savings do not transfer to Witchy's native Rust compiler.
The notes also cover Linux ownership and Wasmtime's native safety contract.

## Open implementation choices

1. Wire encoding and immutable-blob transfer strategy, chosen after measuring
   copying overhead; the identity and immutability contract above is fixed.
2. Warm worker/session granularity and byte budgets, selected from profiles.
3. Minimum supported Linux installation/confinement features; unsupported
   systems must use explicit standalone fallback, not weaker advertised trust.
4. Confirm Linux/macOS rollout order before scheduling adapters. Windows does
   not block initial delivery.
