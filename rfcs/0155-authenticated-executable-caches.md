---
rfc: 0155
title: "Authenticated Executable Caches and Cache-Independent Distribution"
status: proposed
created: 2026-08-24
superseded-by:
related:
  - "0092 (trusted application executables)"
  - "0095 (trusted application installation)"
  - "0126 (capability effects contract)"
tracking: "Proposes cache-independent trusted executables, authenticated local executable caches, guest cache exclusion, and fail-closed fallback."
---

# RFC-0155: Authenticated Executable Caches and Cache-Independent Distribution

## Summary

Witchy currently stores optimized WebAssembly and Wasmtime native compilation
artifacts in mutable per-user cache directories. A Witchy program granted write
access to a parent such as the user's home directory may therefore be able to
replace executable cache state used by a later, more-authoritative program.
Content hashes detect accidental corruption, but do not authenticate a
self-consistent malicious input-to-artifact mapping.

This RFC establishes four independent rules:

1. A trusted executable runs its authenticated embedded WebAssembly without
   consulting or populating any mutable executable cache.
2. Other persistent executable-cache records are accepted only after keyed
   authentication binds the exact input, output, compiler/runtime identity,
   target, configuration, and format.
3. Witchy guest filesystem capabilities cannot access Witchy's cache or cache
   credentials. Supported OS process sandboxes reinforce this rule and confine
   child processes, but are defense in depth rather than the portable trust
   anchor.
4. If authenticated caching is unavailable, execution continues without a
   persistent executable cache. Witchy never silently downgrades to an
   unauthenticated cache and never requires an interactive credential prompt
   merely to execute a program.

The design accepts cache deletion as denial of service. It prevents a
dependency with filesystem-write authority from substituting code that later
runs with another program's greater capability envelope.

## Status of decisions

This RFC is proposed, not accepted. It distinguishes the intended security
contract from implementation choices that still require prototypes.

### Proposed invariants

- Trusted executable launch is cache-independent.
- Persistent executable cache hits are authenticated or rejected.
- Cache authentication failure causes recomputation, never execution.
- Cache metadata never grants capabilities. The current input and launch
  contract remain authoritative.
- Witchy guests cannot obtain a cache-authentication operation or key.
- Missing secure storage causes an uncached run, not a prompt or insecure
  fallback.

### Open implementation choices

- The exact credential backend abstraction and supported headless providers.
- Whether the authentication key is installation-scoped or partitioned by
  compiled-program identity. The record format supports either choice.
- Whether authenticated native-code caching remains in-process or moves behind
  a compiler/cache daemon.
- Which macOS distribution shape, if any, justifies an App Sandbox or XPC
  worker.

## Motivation

Witchy's capability model limits guest authority by linking only the host
operations admitted by the program's launch grant. That boundary is weakened
if a low-authority program can replace executable bytes later loaded on behalf
of a high-authority program.

The relevant attack is:

1. Program A or one of its dependencies receives a writable `Dir` containing
   Witchy's cache, directly or through a parent such as `~/`.
2. A writes a syntactically valid malicious cache entry for program B.
3. B later starts with broader filesystem, network, secret, or process
   capabilities.
4. Witchy accepts the entry as B's compiled code and links B's legitimate host
   capabilities to the attacker's code.

An envelope containing `hash(input)` and `hash(payload)` does not prevent this.
The attacker can construct a mutually consistent mapping:

```text
victim_input_hash -> malicious_payload_hash
malicious_payload_hash -> malicious_payload
```

Wasmtime validation provides a memory-safety boundary for WebAssembly, but it
does not establish that validated WebAssembly is the output of Witchy's
compiler for the selected source. It also deliberately accepts arbitrary valid
WebAssembly, including code chosen to exercise Wasmtime and host-ABI attack
surfaces.

The problem is specific to executable cache state. Witchy does not attempt to
prevent every route by which a program allowed to write arbitrary user files
could influence future native execution, such as editing shell profiles. It is
nevertheless Witchy's responsibility not to add another implicit
write-files-to-execute-code path under its own cache directory.

## Threat model

### In scope

- Malicious Witchy source, WebAssembly, libraries, and build dependencies.
- Any Witchy filesystem capability, including a writable parent of the cache.
- Cross-program and cross-capability cache poisoning.
- Self-consistent cache replacement, truncation, swapping, and replay across
  incompatible compiler/runtime configurations.
- Symlink, hard-link, rename, and verify-then-reopen races in cache handling.
- Child processes launched through `Exec` or `BuildExec` attempting to reach
  the cache.
- Absence, locking, or failure of an OS credential service.
- A malformed cache artifact targeting Wasmtime or Witchy's host ABI.

### Out of scope

- An attacker already able to execute arbitrary native code as the user
  independently of Witchy.
- An attacker able to replace the Witchy runtime, trusted executable, OS
  credential service, kernel, or compiler before the trusted build.
- Preventing a deliberately broad filesystem grant from modifying unrelated
  execution-triggering files outside Witchy's namespace.
- Availability attacks that delete or corrupt cache entries and thereby force
  recompilation.
- A malicious publisher of a trusted executable. The user trusts that complete
  native artifact under RFC-0092.

Native execution as the user normally subsumes access to user-scoped credential
services. It is therefore not meaningful for this RFC to claim protection after
that boundary is crossed.

## Execution models

| Mode | Selected executable input | Persistent-cache policy | Authority source |
|---|---|---|---|
| Interpreter | Current source/package graph | No executable cache | Current checked program and grants |
| Source compiled to WebAssembly | Current source/package graph and compiler configuration | Authenticated only | Current source-derived launch contract |
| Raw `.wasm` | Exact file bytes | Authenticated only | Current validated imports/launch metadata and consumer grants |
| Trusted executable | Authenticated embedded WebAssembly and binding plan | Disabled | Authenticated embedded launch contract and bindings |
| Publisher-supplied precompiled artifact | Signed manifest and exact artifact | Publisher signature; no local mutable substitution | Signed launch contract plus consumer policy |
| Build step | Declared source, inputs, footprint, grants, and tool identities | Separate authenticated build-result namespace | Current build grant |

Build-result caching and executable caching use the same authentication
principles but distinct domains and record types. A build-output record can
never be interpreted as executable-cache authority.

## Design

### Cache-independent trusted executables

`Runtime` exposes a dedicated trusted-executable configuration. It disables
both Witchy's optimized-WebAssembly cache and Wasmtime's native compilation
cache. The launcher validates and executes the exact embedded bytes recovered
from its authenticated payload descriptor and binding plan.

This is intentionally independent of credential-service availability. A
distributed trusted executable must not become less secure, prompt, or stop
working because a local cache key is absent.

This rule may be relaxed only by a later RFC that provides either:

- a publisher-authenticated ahead-of-time artifact covered by the trusted
  executable's own authenticated descriptor, or
- an authenticated local cache meeting every requirement in this RFC and
  demonstrating a material startup benefit.

### Authentication domain

The runtime obtains a random 256-bit cache key from a platform credential
provider. The raw key is available only to trusted native host code and is
never placed in guest linear memory, exposed through a host import, inherited
through environment variables, or written beside the cache.

The preferred provider behavior is non-interactive:

- On first use, create the key if the platform can do so without prompting.
- On subsequent use, request it without demanding user presence.
- If the provider is absent, locked, headless, or would require interaction,
  report caching as unavailable for that process.
- Continue through the uncached compilation path.

A key literally embedded in a compiled native program is not a credential
provider. Keeping it outside guest WebAssembly memory blocks direct guest
loads, but an ordinarily readable executable still reveals it to offline
inspection. Per-program embedded keys may partition failures, but do not
establish authenticity against the stated attacker.

An implementation may partition one installation key into per-program keys:

```text
program_key = BLAKE3-DERIVE-KEY(
    installation_key,
    "witchy executable cache program key v1" || stable_program_identity
)
```

`stable_program_identity` must itself come from authenticated input. A cache
pathname, package display name, or attacker-controlled manifest field is not
sufficient. Partitioning limits accidental cross-use but is not required for
correctness because every record also binds its full input identity.

### Authenticated record

Each cache format has a distinct domain separator. The authenticated canonical
record contains at least:

```text
schema
cache_kind                 # optimized-wasm, native-code, build-result, ...
input_digest               # digest of exact source graph or input wasm
artifact_digest            # digest of exact payload bytes
compiler_identity
runtime_identity
optimizer_identity
target_triple
codegen_configuration
feature_set
launch_contract_digest
payload_length
```

The tag is a keyed BLAKE3 digest over the canonical encoding and the artifact
digest:

```text
tag = BLAKE3-KEYED(cache_key, domain_separator || canonical_record)
```

The cache lookup filename may remain content-addressed for deduplication, but
the filename is not authenticated evidence. Content addressing answers whether
bytes match a name; the tag answers whether Witchy's trusted compiler approved
the complete input-to-artifact relationship.

Unknown schemas, fields, algorithms, cache kinds, compiler/runtime identities,
or feature sets fail closed. Cache keys and record formats carry explicit
epochs so a security release can invalidate all older records.

### Cache insertion

Only trusted host compiler code constructs an authenticated record:

1. Read and hash the exact input bytes or canonical source graph.
2. Compile or optimize those bytes in the trusted host.
3. Hash the resulting artifact.
4. Construct and authenticate the canonical record.
5. Write artifact and record to temporary files in the cache directory.
6. Flush as required by the cache's durability contract and publish atomically.

There is no guest-callable `sign_cache`, `put_compiled_artifact`, or equivalent
operation. Otherwise the host becomes a signing oracle for attacker-selected
input-to-output assertions. A future daemon accepts inputs and compilation
requests, not claimed compilation results.

### Cache lookup

Lookup proceeds as follows:

1. Hash the exact current input and derive all compiler/runtime configuration
   independently of cache contents.
2. Open the candidate record and artifact without following unexpected links.
3. Bound their lengths before allocation or parsing.
4. Decode the canonical record using a memory-safe parser.
5. Compare every contextual field, verify the keyed tag, then hash the exact
   artifact bytes and compare `artifact_digest`.
6. Validate ordinary WebAssembly through Wasmtime before instantiation.
7. Consume the same opened bytes that were authenticated. Do not verify a path
   and reopen it.

For engine-native serialized artifacts, authentication must complete before
calling any unsafe or trust-requiring deserialization API. If Wasmtime's cache
integration cannot provide this ordering, Witchy disables that cache until it
can wrap or replace the integration safely.

Cache metadata never supplies imports, capability footprints, launch bindings,
grants, or resource identities. Witchy derives those from current authenticated
input and current consumer policy. Cached metadata may duplicate them only for
an equality check that fails closed.

### Guest cache exclusion

The cache and cache credential namespaces are reserved host resources. Witchy
does not mint guest `Dir` or `File` capabilities that directly identify them.
Every guest filesystem operation denies traversal, listing, reading, writing,
creation, truncation, unlinking, linking, or renaming that resolves into those
namespaces.

This restriction applies even when a user grants a parent directory such as
the home directory. Implementations must test object identity and open-handle
behavior rather than relying only on lexical path prefixes. Witchy's own
compiler and package tooling use a private host path unavailable through guest
imports.

This portable rule is defense in depth. The MAC remains authoritative because
filesystem mediation can contain implementation mistakes and cannot constrain
an unrelated same-user native process.

### OS process confinement

Where supported, Witchy runs guest execution in a worker process whose OS
sandbox excludes cache and credential storage. The worker receives only the
already-validated module and explicit capability resources. Cache file
descriptors are closed before guest execution.

On Linux, Landlock restrictions must be applied before guest capability handles
are opened and are inherited by child processes. Landlock is an allow-list: a
policy that grants an entire parent hierarchy cannot express a negative child
exception merely by omitting the cache. Witchy must therefore use sufficiently
narrow grant roots, place protected state outside allowed hierarchies, hide it
through a separate filesystem view, or decline to claim cache exclusion for
that run.

On macOS, the supported App Sandbox model requires signed applications or
helpers with entitlements. Dynamic access normally uses security-scoped URLs or
bookmarks, and XPC is the supported privilege-separation mechanism. Witchy may
use that architecture for an appropriately packaged distribution, but must not
depend on deprecated `sandbox-exec` profiles for the portable guarantee.

Other platforms may provide equivalent worker confinement. Lack of such a
facility does not permit unauthenticated cache use; it removes one
defense-in-depth layer.

### Failure and override behavior

Cache behavior has three user-visible states:

| State | Behavior |
|---|---|
| `authenticated` | Read and write authenticated persistent records |
| `unavailable` | Compile normally; use only process-memory caching |
| `unsafe-explicit` | Permit legacy unauthenticated caching after an explicit warning |

`unavailable` is the automatic fallback. Credential prompts are not part of
ordinary program startup. Cache corruption is handled as a miss, optionally
with a bounded diagnostic; it never changes program authority.

An escape hatch, if retained, must be unmistakably unsafe. The proposed forms
are `--unsafe-trust-cache` and `WITCHY_UNSAFE_TRUST_CACHE=1`. The shorter
`TRUST_CACHE=true` is rejected because it obscures both scope and consequence.
Security-sensitive release and trusted-executable paths ignore the override.

The environment override is intended for developer and hermetic CI use only.
It must not be inherited into `Exec`/`BuildExec` children, and package metadata
or guest-controlled environment values cannot enable it.

## Attack analysis

| Attack | Required defenses | Result |
|---|---|---|
| Replace artifact but keep record | Artifact digest and MAC | Miss and recompile |
| Replace record and artifact consistently | Secret-key MAC | Miss and recompile |
| Poison another program's entry | Bound input and launch-contract digests; optional program-key partition | Miss and recompile |
| Replay old compiler output | Compiler/runtime/configuration identities and epoch | Miss and recompile |
| Reach cache through broad `Dir` | Guest cache exclusion; OS worker where available | Denied; MAC remains backup |
| Reach cache through child process | Worker sandbox inherited across child execution | Denied where supported; MAC remains authoritative |
| Extract embedded key | No embedded cache key | No key present |
| Use host as signing oracle | No guest cache-authentication API; trusted compiler constructs records | No attacker-selected assertion authenticated |
| Symlink or rename between verification and use | Descriptor-relative no-follow opens and same-open-byte consumption | Replacement not consumed |
| Feed malformed native artifact to unsafe deserializer | Authenticate and bound before deserialization | Rejected before unsafe boundary |
| Delete cache | Out of scope availability attack | Recompile safely |
| Exploit Wasmtime with authenticated compiler output | Runtime hardening, updates, and fuzzing outside this cache protocol | Not solved by cache authentication |
| Compromised library uses containing module's valid capabilities | Dependency isolation or separate instances | Not solved by cache authentication |

## Performance

Trusted executables initially pay native compilation on every process launch.
This is a deliberate short-term cost until an authenticated ahead-of-time or
local-cache path demonstrates equivalent security.

For authenticated cache hits, the incremental work is one bounded record parse,
one keyed hash, and hashing bytes that must already be read. This should be much
smaller than Binaryen or Cranelift compilation, but implementation promotion
requires measurement rather than assumption.

The acceptance evidence must report:

- cold and warm startup for representative small, medium, and large modules;
- cache-hit hashing and verification time separately;
- source-to-WebAssembly, optimized-WebAssembly, and native compilation costs;
- trusted-executable startup before and after cache independence;
- authenticated, unavailable, corrupted, and unsafe-explicit modes;
- desktop credential provider and headless/no-provider behavior.

A daemon may improve performance by retaining validated modules in memory and
serializing compilation across clients. It is not required by this RFC and may
not weaken the fallback behavior or expose a signing oracle.

## Compatibility and migration

Existing unauthenticated entries are untrusted and are not migrated by attaching
a tag. Witchy recompiles them from current inputs and emits new authenticated
records. Legacy cache directories may be removed lazily after bounded retention.

The cache format and key epoch are versioned independently. Losing or rotating
the credential invalidates cache entries but never prevents program execution.
Copying a cache to another account or machine produces misses unless the
corresponding protected credential was deliberately migrated.

Trusted executable behavior changes only in performance: launch semantics,
embedded bindings, capability checks, and output remain unchanged.

## Acceptance criteria

Implementation is complete only when all of the following are demonstrated:

1. Trusted executable tests run with an absent cache root and leave it absent.
2. A valid malicious optimized-WebAssembly envelope with the expected input
   hash but no valid MAC is rejected.
3. A mutually consistent malicious record and artifact are rejected.
4. Cross-program, cross-target, cross-configuration, and old-epoch replays are
   rejected.
5. Invalid and unavailable credentials run successfully without persistent
   cache access or interactive prompts.
6. Cache records are authenticated before any native-artifact deserialization.
7. Capability footprints, imports, bindings, and grants are derived outside the
   cache and mismatches fail closed.
8. Parent-directory grants, direct grants, symlinks, hard links, rename,
   truncation, deletion, and concurrent replacement are covered by adversarial
   tests.
9. On Linux systems with a supported Landlock ABI, guest workers and their
   children cannot access protected state through the granted roots.
10. Unsupported or unexpressible OS confinement is reported as unavailable,
    not silently represented as enforced.
11. Unsafe cache mode requires an explicit unsafe control and emits a clear
    diagnostic; trusted executables ignore it.
12. Focused performance evidence quantifies startup and cache-hit overhead.
13. Fuzz targets cover canonical record decoding, length bounds, algorithm and
    schema dispatch, and cache lookup races.

## Rollout

### Phase 1: close trusted distribution

- Add a cache-independent trusted-executable runtime configuration.
- Prove no optimized-WebAssembly or Wasmtime cache state is read or written.
- Measure the startup regression.

### Phase 2: portable authenticated optimized-WebAssembly cache

- Introduce the credential-provider abstraction and keyed canonical record.
- Implement non-interactive unavailable fallback.
- Reserve cache/key namespaces from guest capabilities.
- Reject all legacy optimized-WebAssembly entries.

### Phase 3: native-cache decision

- Audit Wasmtime cache lookup and deserialization ordering.
- Wrap native artifacts with pre-deserialization authentication, replace the
  integration, or leave persistent native caching disabled.
- Measure whether a broker/daemon is justified.

### Phase 4: OS worker confinement

- Add Linux Landlock worker confinement and inheritance tests.
- Prototype supported macOS App Sandbox/XPC packaging before committing to that
  distribution shape.
- Add equivalent platform workers where support and maintenance justify them.

## Alternatives

### Disable every persistent executable cache

This is the smallest security mechanism and the universal fallback. Making it
the only mode imposes repeated Binaryen and Cranelift costs even where cheap
authentication can retain performance.

### Content addressing without authentication

Rejected as a security boundary. It detects bytes that do not match their
claimed digest, but an attacker can replace both the mapping and content with a
consistent malicious pair.

### Embed a key in each compiled Witchy program

Rejected as the primary authenticator. It keeps the key outside guest linear
memory and limits compromise to one program, but normal native executable
inspection can recover it. It may be useful only as defense in depth under a
separately enforced OS sandbox.

### Rely only on guest path denial

Rejected as the sole boundary. It is cheap and worthwhile defense in depth, but
aliases, broad parent grants, child processes, and implementation defects make
cryptographic authentication valuable.

### Rely only on platform sandboxing

Rejected as the portable boundary. Landlock cannot subtract a protected child
from an allowed parent hierarchy, and supported macOS App Sandbox use entails a
different signed application/helper distribution model.

### Always require an unlocked keyring

Rejected. A cache optimization must not prevent ordinary execution, break
headless use, or introduce surprise authentication prompts.

### Cache daemon

Deferred, not rejected. A daemon can own credentials, compile inputs itself,
keep hot modules in memory, and avoid exposing unsafe native artifacts to
clients. It adds lifecycle, protocol, upgrade, availability, and platform
integration complexity. The authenticated record and fail-closed client policy
remain useful whether or not a daemon is added.

## Drawbacks

- Credential-provider integration is platform-specific and headless behavior is
  necessarily less uniform than ordinary file I/O.
- Authentication adds cache format, key rotation, and migration machinery.
- Trusted executable startup becomes slower until a secure optimized path is
  implemented.
- A worker-process sandbox complicates capability transfer, output, debugging,
  profiling, and process supervision.
- Guest cache exclusion reserves a namespace that a user cannot deliberately
  grant to Witchy guest code through ordinary filesystem capabilities.
- The unsafe escape hatch creates a mode that documentation and diagnostics
  must keep visibly separate from the default security contract.

## Prior art

- Linux Landlock uses inherited, stackable allow-list restrictions for file
  hierarchies and child processes:
  <https://www.kernel.org/doc/html/latest/userspace-api/landlock.html>
- Apple App Sandbox uses signed entitlements and security-scoped resource access;
  XPC is the supported privilege-separation mechanism:
  <https://developer.apple.com/documentation/security/app-sandbox>
- Windows DPAPI provides non-interactive user-scoped data protection and an
  explicit no-UI mode:
  <https://learn.microsoft.com/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata>
- The Secret Service API permits non-interactive access to unlocked collections
  while representing required prompts explicitly:
  <https://specifications.freedesktop.org/secret-service/latest/>

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
