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
tracking: "Proposes cache-independent trusted executables, authenticated local executable caches, fail-closed fallback, and optional guest cache exclusion."
---

# RFC-0155: Authenticated Executable Caches and Cache-Independent Distribution

## Summary

Witchy currently stores optimized WebAssembly and Wasmtime native compilation
artifacts in mutable per-user cache directories. A Witchy program granted write
access to a parent such as the user's home directory may therefore be able to
replace executable cache state used by a later, more-authoritative program.
Content hashes detect accidental corruption, but do not authenticate a
self-consistent malicious input-to-artifact mapping.

This RFC establishes three required rules and one optional hardening layer:

1. A trusted executable runs its authenticated embedded WebAssembly without
   consulting or populating any mutable executable cache.
2. Other persistent executable-cache records are accepted only after keyed
   authentication binds the exact input, output, compiler/runtime identity,
   target, configuration, and format.
3. If authenticated caching is unavailable, execution continues without a
   persistent executable cache. Witchy never silently downgrades to an
   unauthenticated cache and never requires an interactive credential prompt
   merely to execute a program.
4. Where it can be implemented without weakening ordinary capability semantics,
   Witchy guest filesystem capabilities exclude Witchy's cache and cache
   credentials as defense in depth. This exclusion is not the portable
   integrity anchor and does not replace authentication.

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
- Witchy guests cannot obtain a cache-authentication operation or key. A native
  child is covered by this claim only when the selected authenticator and OS
  confinement can exclude that child explicitly.
- Missing secure storage causes an uncached run, not a prompt or insecure
  fallback.

### Result of the adversarial composition review

The review treats a control as required only when removing it enables an attack
that the remaining controls do not stop. The resulting cache-integrity core is:

1. an authenticator unavailable to the in-scope attacker and no
   attacker-controlled signing oracle;
2. authentication over the complete compilation relationship;
3. producer-side input freezing and consumer-side exact-byte use;
4. authentication and bounds checks before any trust-requiring native parse;
5. capability authority derived independently from current trusted input and
   current launch policy; and
6. cache-off fallback with no production downgrade switch.

Trusted executables use a disjoint mitigation: they do not consult persistent
executable caches. They therefore do not depend on any member of the
authenticated-cache core.

Program identity is deliberately absent from the core. Two programs may safely
share an artifact when their exact compilation input and recipe are identical,
because the artifact carries no grant, binding, footprint, or other authority.
Per-program key derivation and mandatory cross-program cache misses would add
complexity without closing an additional attack.

### Open implementation choices

- The exact authenticator backend abstraction and supported headless providers.
- Whether authenticated native-code caching remains in-process or moves behind
  a compiler/cache daemon.
- Which macOS distribution shape, if any, justifies an App Sandbox or XPC
  worker.
- Whether guest cache exclusion is sufficiently cheap and portable to ship as
  general hardening rather than only as part of a native-child containment
  architecture.

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
- Child processes launched through `Exec` or `BuildExec` when the selected
  platform confinement explicitly claims to contain them.
- Absence, locking, or failure of an OS credential service.
- A malformed cache artifact targeting Wasmtime or Witchy's host ABI.

### Out of scope

- An attacker already able to execute arbitrary native code as the user
  independently of Witchy.
- A child launched through `Exec` or `BuildExec` on a platform or launch mode
  that cannot isolate it from both the cache and authenticator. Such a grant is
  explicitly native-user authority, not contained Witchy authority.
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
that boundary is crossed. In particular, an ordinary same-user credential store
is not by itself a valid authenticator against an in-scope native child: the
child may be able to retrieve the same key. Covering native children requires
one of:

- enforced process isolation from both cache storage and the credential
  service;
- a non-exportable authentication operation restricted to trusted Witchy code;
- a compile-only broker running under a distinct OS security identity; or
- treating the `Exec`/`BuildExec` grant as an explicit crossing of this threat
  boundary.

Disabling cache use only for the process that launched the child is
insufficient. A surviving child could obtain a persistent same-user key or
poison state consumed by a later process.

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

The runtime obtains an authentication operation from a platform authenticator.
The simplest qualifying implementation is a random 256-bit key in a credential
provider that is inaccessible to every attacker in the active threat model.
The raw key is available only to trusted native host code and is never placed
in guest linear memory, exposed through a host import, inherited through
environment variables, or written beside the cache.

An implementation that claims containment of same-user native children must
not assume that a user-scoped credential store distinguishes the trusted
Witchy host from those children. It must demonstrate that distinction or use a
non-exportable operation or broker. If it cannot, native-child containment is
unavailable on that platform.

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

Per-program key derivation is not part of the security contract. Complete
record binding prevents substitution across distinct compilation inputs. Safe
reuse across programs with identical compilation inputs is desirable, while
deriving every key from one installation secret does not contain compromise of
that secret.

### Authenticated record

Each cache format has a distinct domain separator. The authenticated canonical
record contains at least:

```text
schema
cache_kind                 # optimized-wasm, native-code, build-result, ...
input_digest               # digest of exact source graph or input wasm
artifact_digest            # digest of exact payload bytes
recipe_digest               # canonical compiler/runtime/tool/target/config identity
payload_length
```

The canonical recipe covered by `recipe_digest` includes the compiler,
runtime, optimizer executable, target, feature set, code-generation settings,
cache epoch, and only those trusted launch facts that change generated bytes.
It never includes current grants as a substitute for checking them. These are
one binding obligation rather than independent mitigations.

Program name, installation path, and current capability grant are not cache-key
inputs unless they actually change generated bytes. A cache artifact is a pure
compilation result, not a principal. Authority-bearing launch facts are checked
outside the cache, so identical code may be deduplicated without transferring
authority between consumers.

The tag is a keyed BLAKE3 digest over the canonical encoding and the artifact
digest:

```text
tag = BLAKE3-KEYED(cache_key, domain_separator || canonical_record)
```

The cache lookup filename may remain content-addressed for deduplication, but
the filename is not authenticated evidence. Content addressing answers whether
bytes match a name; the tag answers whether Witchy's trusted compiler approved
the complete input-to-artifact relationship.

Unknown schemas, fields, algorithms, cache kinds, or recipe identities fail
closed. Cache keys and record formats carry explicit epochs so a security
release can invalidate all older records.

### Cache insertion

Only trusted host compiler code constructs an authenticated record:

1. Open and freeze the exact input bytes or canonical source graph, then hash
   that frozen representation.
2. Compile or optimize that same representation without reopening mutable
   source paths.
3. Hash the resulting artifact.
4. Construct and authenticate the canonical record.
5. Write artifact and record to temporary files in the cache directory.
6. Flush as required by the cache's durability contract and publish atomically.

There is no guest-callable `sign_cache`, `put_compiled_artifact`, or equivalent
operation. Otherwise the host becomes a signing oracle for attacker-selected
input-to-output assertions. A future daemon accepts inputs and compilation
requests, not claimed compilation results.

Every external compiler or optimizer is part of the trusted producer. Witchy
must resolve it through trusted configuration, open the exact executable whose
identity is included in `recipe_digest`, and prevent an ambient `PATH` change or
verify/execute race from substituting another tool. A version string printed by
an arbitrary `wasm-opt` found on `PATH` is not an authenticated optimizer
identity.

### Cache lookup

Lookup proceeds as follows:

1. Freeze and hash the exact current input and derive the canonical recipe
   independently of cache contents.
2. Open the candidate record and artifact without following unexpected links.
3. Bound their lengths before allocation or parsing.
4. Decode the canonical record using a memory-safe parser.
5. Compare every contextual field, verify the keyed tag, then hash the exact
   artifact bytes and compare `artifact_digest`.
6. Validate ordinary WebAssembly through Wasmtime before instantiation.
7. Consume the same opened bytes that were authenticated. Do not verify a path
   and reopen it.

For engine-native serialized artifacts, authentication and length bounds must
complete before calling any unsafe or trust-requiring deserialization API. The
implementation must also document why artifacts emitted by the authenticated
producer satisfy that API's safety preconditions. Authentication proves
provenance; it does not prove memory safety or make arbitrary native bytes safe.
If Wasmtime's cache integration cannot provide both the required ordering and a
defensible producer-to-deserializer safety contract, Witchy disables that cache
until it can wrap or replace the integration safely.

Cache metadata never supplies imports, capability footprints, launch bindings,
grants, or resource identities. Witchy derives those from current authenticated
input and current consumer policy. Cached metadata may duplicate them only for
an equality check that fails closed.

### Optional guest cache exclusion

The cache and cache credential namespaces are reserved host resources. As
optional defense in depth, Witchy may refuse to mint guest `Dir` or `File`
capabilities that directly identify them. A complete implementation denies
traversal, listing, reading, writing, creation, truncation, unlinking, linking,
or renaming that resolves into those namespaces.

This restriction applies even when a user grants a parent directory such as
the home directory. Implementations must test object identity and open-handle
behavior rather than relying only on lexical path prefixes. Witchy's own
compiler and package tooling use a private host path unavailable through guest
imports.

This rule is not required for cache integrity when complete authentication and
race-free loading hold. It reduces denial of service and exposure to verifier
mistakes or legacy formats, but those benefits do not justify distorting an
explicit broad filesystem grant or delaying the authentication core. It becomes
mandatory only when Witchy makes a separate claim that a native child remains
contained from cache storage and credentials.

### OS process confinement

Where supported, Witchy may run guest execution in a worker process whose OS
sandbox excludes cache and credential storage. The worker receives only the
already-validated module and explicit capability resources. Cache file
descriptors are closed before guest execution.

For ordinary WebAssembly without native child authority, this is defense in
depth and is not required for cache integrity once authenticated exact-byte
loading holds. If Witchy claims that an `Exec` or `BuildExec` child remains
inside the threat boundary, however, OS confinement or an equivalent broker is
mandatory and must exclude both cache files and the authentication operation.

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

Cache behavior has two user-visible states:

| State | Behavior |
|---|---|
| `authenticated` | Read and write authenticated persistent records |
| `unavailable` | Compile normally; use only process-memory caching |

`unavailable` is the automatic fallback. Credential prompts are not part of
ordinary program startup. Cache corruption is handled as a miss, optionally
with a bounded diagnostic; it never changes program authority. There is no
runtime flag or environment variable that enables unauthenticated executable
caching. Benchmarks that need a legacy cache must use a separately built test
fixture outside the production security contract.

## Minimality and composition

The authenticated-cache core is:

```text
protected authenticator and trusted-only record production
+ complete canonical relationship binding
+ frozen producer inputs and same-open consumer bytes
+ bounded envelope parsing and authentication before artifact consumption
+ authority derived independently of cache contents
+ cache-off fallback without a downgrade switch
```

The first line is one obligation, not two stackable mitigations. A strong key is
useless if the attacker can ask trusted code to authenticate arbitrary output,
and a closed producer is useless if an attacker can forge its records. Likewise,
the canonical relationship is one binding obligation: splitting input,
artifact, recipe, target, and epoch into independent controls does not provide
independent security.

Each remaining line blocks a different failure after the preceding lines hold:

| Removed obligation | Attack still available with every other obligation |
|---|---|
| Protected authenticator and trusted producer | Forge a self-consistent malicious record, or use the host as a signing oracle |
| Complete relationship binding | Replay an authentic artifact for a different input, toolchain, target, configuration, or epoch |
| Frozen producer inputs and same-open consumer bytes | Swap source after hashing or artifact after verification |
| Bounded envelope parse and pre-consumption authentication | Reach an artifact parser, especially a native unsafe deserializer, with attacker-controlled payload bytes before rejection |
| Authority independent of cache | Reuse stale or mismatched cached grants and bindings even with authentic code |
| Cache-off fallback without downgrade | Turn credential failure into acceptance of unauthenticated state |

No proper subset of these obligations covers all listed attacks. Content
addressing, per-program keys, program identity, guest path denial, and OS
sandboxing for a WebAssembly guest without native-child authority do not replace
any member of the core.

The controls compose by execution mode rather than accumulating globally:

| Execution mode | Required mitigation |
|---|---|
| Trusted executable | Cache independence only |
| Source, raw Wasm, or embedded-module persistent cache | Complete authenticated-cache core |
| Native serialized-artifact cache | Complete core plus a documented unsafe-deserialization safety contract |
| No persistent cache | Exact current inputs and current authority checks; authentication controls are not applicable |
| Native child claimed as contained | Core plus OS or broker isolation from both cache and authenticator |

A broker or daemon can replace the local authenticator and producer mechanism
only if it compiles frozen inputs itself under a distinct security boundary. A
daemon that accepts caller-supplied artifacts or caller-asserted digests merely
moves the signing oracle into another process.

Trusted executables form a separate execution mode. Cache independence alone
closes their mutable-cache substitution path and deliberately avoids depending
on the authentication core. Once an authenticated trusted-executable cache is
implemented and proven, cache independence may become a transitional rather
than permanent rule.

## Attack analysis

| Attack | Required defenses | Result |
|---|---|---|
| Replace artifact but keep record | Artifact digest and MAC | Miss and recompile |
| Replace record and artifact consistently | Secret-key MAC | Miss and recompile |
| Substitute across distinct program inputs | Bound exact-input and recipe digests | Miss and recompile |
| Reuse identical code for another program | Authority remains outside the cache | Safe hit; no authority transfers |
| Replay old compiler output | Recipe identity and epoch | Miss and recompile |
| Reach cache through broad `Dir` | Complete authenticated-cache core | Substitution rejected; optional exclusion may deny access earlier |
| Reach cache through child process | Isolation from both cache and authenticator, or explicit native-authority boundary | Denied only when the complete isolation claim holds |
| Extract embedded key | No embedded cache key | No key present |
| Use host as signing oracle | No guest cache-authentication API; trusted compiler constructs records | No attacker-selected assertion authenticated |
| Substitute `wasm-opt` or another compiler tool | Trusted resolution and exact tool identity in recipe | Untrusted producer never signs output |
| Change source between hashing and compilation | Freeze once and compile the hashed representation | Record binds the bytes actually compiled |
| Symlink or rename between verification and use | Descriptor-relative no-follow opens and same-open-byte consumption | Replacement not consumed |
| Feed malformed native artifact to unsafe deserializer | Authenticate and bound before deserialization | Rejected before unsafe boundary |
| Delete cache | Out of scope availability attack | Recompile safely |
| Exploit Wasmtime with authenticated compiler output | Runtime hardening, updates, and fuzzing outside this cache protocol | Not solved by cache authentication |
| Compromised library uses containing module's valid capabilities | Dependency isolation or separate instances | Not solved by cache authentication |
| Same-user native child retrieves credential key | Required OS/broker isolation or explicit native-authority boundary | Never claimed safe through credential storage alone |

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
- authenticated, unavailable, and corrupted modes;
- desktop authenticator and headless/no-provider behavior.

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

The cache-integrity core is complete only when all of the following are
demonstrated:

1. Trusted executable tests run with an absent cache root and leave it absent.
2. A valid malicious optimized-WebAssembly envelope with the expected input
   hash but no valid MAC is rejected.
3. A mutually consistent malicious record and artifact are rejected.
4. Cross-input, cross-target, cross-configuration, and old-epoch replays are
   rejected. Identical-input cross-program reuse remains safe and permitted.
5. Invalid and unavailable credentials run successfully without persistent
   cache access or interactive prompts.
6. Cache records are length-bounded and authenticated before artifact
   consumption, and native deserialization additionally has a documented
   producer-to-deserializer safety contract.
7. Capability footprints, imports, bindings, and grants are derived outside the
   cache and mismatches fail closed.
8. Production binaries expose no unauthenticated executable-cache override.
9. External compiler and optimizer identity is authenticated, and ambient
    `PATH` substitution cannot produce an authenticated record.
10. The representation hashed for insertion is the exact representation
    compiled, with no mutable-path reopen between those operations.
11. Focused performance evidence quantifies startup and cache-hit overhead.
12. Fuzz targets cover canonical record decoding, length bounds, algorithm and
    schema dispatch, and cache lookup races.

Optional defense-in-depth claims have their own acceptance gates and do not
delay the core. A claim is made only if its corresponding gate passes:

1. If shipped, guest cache exclusion covers parent-directory and direct grants,
   symlinks, hard links, rename, truncation, deletion, open handles, and
   concurrent replacement.
2. Native-child containment proves the child cannot access either cache
   storage or the authentication operation.
3. A Linux Landlock claim proves guest workers and descendants cannot reach
   protected state through every granted root.
4. Unsupported or unexpressible OS confinement is reported as unavailable,
   never silently represented as enforced.

## Rollout

### Phase 1: close trusted distribution

- Add a cache-independent trusted-executable runtime configuration.
- Prove no optimized-WebAssembly or Wasmtime cache state is read or written.
- Measure the startup regression.

### Phase 2: portable authenticated optimized-WebAssembly cache

- Introduce the authenticator abstraction and keyed canonical record.
- Implement non-interactive unavailable fallback.
- Pin and authenticate every external optimizer/compiler tool.
- Reject all legacy optimized-WebAssembly entries.

### Phase 3: native-cache decision

- Audit Wasmtime cache lookup and deserialization ordering.
- Wrap native artifacts with pre-deserialization authentication, replace the
  integration, or leave persistent native caching disabled.
- Measure whether a broker/daemon is justified.

### Phase 4: optional hardening and native-child decision

- Reserve cache/key namespaces from guest capabilities only if the mechanism is
  complete and does not distort intended broad grants; do not make this a
  prerequisite for authenticated caching.
- Add Linux Landlock worker confinement and inheritance tests if native-child
  containment remains a product goal.
- Prototype supported macOS App Sandbox/XPC packaging before committing to that
  distribution shape.
- On platforms without qualifying containment, document `Exec`/`BuildExec` as
  explicit native-user authority rather than claiming cache isolation.

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

### Derive per-program keys from one installation key

Rejected as a required mitigation. Complete record binding prevents
substitution across distinct inputs, identical-input reuse is safe, and
compromise of the installation key compromises every deterministically derived
child key.

### Rely only on guest path denial

Rejected as the sole boundary. It can be worthwhile defense in depth when it is
complete and cheap, but aliases, broad parent grants, child processes, and
implementation defects prevent it from replacing authentication.

### Rely only on platform sandboxing

Rejected as the portable boundary. Landlock cannot subtract a protected child
from an allowed parent hierarchy, and supported macOS App Sandbox use entails a
different signed application/helper distribution model.

### Always require an unlocked keyring

Rejected. A cache optimization must not prevent ordinary execution, break
headless use, or introduce surprise authentication prompts.

### Permit an explicit unauthenticated-cache override

Rejected. Such a control directly negates the security invariant and creates a
new environment/configuration injection target. Dedicated benchmark fixtures
can measure legacy behavior without shipping the bypass in production.

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
- If enabled, guest cache exclusion reserves a namespace that a user cannot
  deliberately grant to Witchy guest code through ordinary filesystem
  capabilities.
- Containing native children may require a broker, separate OS identity, or
  platform packaging that is substantially more complex than a credential
  lookup.

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
