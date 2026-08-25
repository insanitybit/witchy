---
rfc: 0157
title: "Trusted Publishers and Executable-State Writes"
status: accepted
created: 2026-08-25
superseded-by:
related:
  - "0092 (trusted application executables, terminology superseded)"
  - "0095 (trusted application installation, rejected)"
  - "0126 (capability effects contract)"
  - "0155 (authenticated executable caches, rejected)"
  - "0156 (AOT native trusted executables, rejected)"
tracking: "Accepted policy. The one-cut trusted-exe to trusted-publisher rename, ordinary-cache restoration, and documentation updates remain to be implemented."
---

# RFC-0157: Trusted Publishers and Executable-State Writes

## Summary

Rename Witchy's `trusted-exe` deployment mode to `trusted-publisher`. The
consumer trusts the publisher as the application's first-order principal: the
publisher chooses the root capability bindings and packages the standard Witchy
enforcement substrate. That trust does not flow transitively to dependencies.
Guest code remains inside Wasmtime and receives authority only through Witchy's
ordinary capability delegation.

Accept that write authority over files later consumed as executable state can
influence future execution. An actor able to modify Witchy's executable cache is
therefore outside the cache-integrity threat model. Witchy keeps corruption and
compatibility checks, but does not add keyed cache authentication, credential
storage, cache-specific capability exclusions, or cache-independent publisher
applications.

This decision rejects RFC-0095, RFC-0155, and RFC-0156, and supersedes the trust
terminology of RFC-0092. It does not approve publisher-supplied native extensions
or arbitrary native payloads.

## Motivation

### Name the principal that is trusted

`trusted-exe` suggests that every component in the executable is trusted. That
is not the useful Witchy contract. A consumer trusts publisher D0 to identify
the application, select its root resources, and package it honestly. D0 may use
external libraries that the consumer does not trust and that D0 does not intend
to grant root authority.

Those libraries remain guest code. They cannot construct capabilities and gain
authority only when D0's code explicitly passes capability values or effectful
callbacks to them. The Wasmtime boundary, host import linking, capability
attenuation, and trap behavior continue to apply to the whole guest program.

The name `trusted-publisher` states the first-order trust decision without
erasing the internal sandbox.

### Broad writes have persistent consequences

A filesystem write is not limited to the process performing it. Files can later
be interpreted by shells, build tools, editors, dynamic loaders, package
managers, or executable caches. A program allowed to write an arbitrary user
tree can commonly influence later native execution through many paths that are
not specific to Witchy.

Hiding selected cache paths or authenticating one cache does not turn broad
write authority into a non-persistent capability. It creates a partial shadow
policy while leaving equivalent execution paths available. Witchy's security
boundary is instead the delegated filesystem root:

- A `Dir[Write]` rooted at a project cannot modify a cache outside that root.
- A `Dir[Write]` whose root contains executable state authorizes modification of
  that state and accepts its future-execution consequences.
- Callers that do not accept those consequences must delegate narrower roots.

This is an authority statement, not a claim that every write immediately
executes code.

## Trust model

### Trusted

For a `trusted-publisher` application, the consumer trusts the publisher to:

1. choose and disclose the root capability bindings;
2. package the standard Witchy launcher, runtime, and compiler output honestly;
3. bind the checked guest Wasm and launch contract into the published artifact;
4. prevent guest dependencies and dependency build steps from replacing the
   launcher, binding plan, or final native container; and
5. sign or distribute the artifact through whatever identity mechanism caused
   the consumer to trust that publisher.

Witchy's launcher, runtime, Wasmtime engine, compiler, and packaging path remain
part of the enforcement trusted computing base. A future installer may verify
their identity independently, but this RFC does not design that installer.

### Untrusted and confined

The following remain untrusted:

- transitive Witchy dependencies and their guest behavior;
- application inputs, network peers, and data files;
- mirrors and transports not covered by the publisher's identity mechanism;
- files outside delegated capability roots; and
- portable Wasm supplied to a consumer-owned Witchy host.

Untrusted guest code continues to enter Wasmtime through validated Wasm. This
RFC does not permit a dependency or portable module to provide serialized native
code to an unsafe deserializer.

### Explicitly outside the cache-integrity threat model

An actor that can write Witchy's executable cache, or a parent directory that
contains it, can influence future execution. The same is true for other
execution-triggering files under that writable root. Witchy does not promise
cross-program integrity after granting that authority.

Same-user arbitrary native execution is outside this boundary for the same
reason. Cache deletion and corruption remain availability and reliability
events, not privilege escalation from a lesser authority.

## Design

### One-cut terminology migration

The implementation replaces the deployment term in one cut:

```text
trusted-exe                         -> trusted-publisher
--target trusted-exe                -> --target trusted-publisher
[targets.trusted-exe]               -> [targets.trusted-publisher]
Runtime::trusted_executable()       -> ordinary publisher launch path
```

The old CLI target and manifest table fail with a diagnostic naming the new
spelling. There is no compatibility alias. Historical RFC text remains
historical; current specifications, examples, diagnostics, tests, and project
manifests use the new term.

The artifact remains a native launcher containing checked Wasm and a checked
binding plan. The rename does not change guest semantics or grant additional
host imports.

### Ordinary executable caching

Publisher applications use the same optimized-Wasm and Wasmtime compilation
cache policy as ordinary source and portable-Wasm execution. The dedicated
cache-independent runtime introduced by commit `4923baafb` is removed.

Cache entries retain the checks required for reliable operation:

- exact cache format and version compatibility;
- complete lengths and bounded parsing;
- content digests or checksums used to detect corruption;
- Wasmtime engine and target compatibility; and
- safe rejection followed by recomputation when an entry is invalid.

These checks are not described as authentication against a writer. No cache key,
MAC, credential-service integration, prompt, cache-specific hidden path, or
guest cache exclusion is added.

This RFC does not require preserving every current cache implementation. A cache
may remain disabled when its performance value is unmeasured or its library API
cannot meet Rust safety requirements. That is an engineering decision, not a
defense against an in-scope cache writer.

### Native artifacts

Portable and guest-controlled inputs continue through safe Wasm validation and
compilation. A checksum does not make arbitrary serialized native bytes safe for
`Module::deserialize`.

RFC-0156's AOT design is rejected rather than repaired here. A future AOT RFC
must begin with matched startup measurements and preserve the existing Witchy VM
state, capability linker, limits, cleanup, output, exit, and trap path. It must
also state why every native byte satisfies Wasmtime's unsafe deserialization
contract. Cache poisoning is not a valid motivation under this RFC.

## Minimal implementation

1. Rename the target, manifest table, runtime terminology, diagnostics, tests,
   examples, and current documentation to `trusted-publisher` in one change.
2. Remove the publisher-only no-cache runtime and launch publisher applications
   through the ordinary cache-enabled runtime path.
3. Retain descriptor, embedded-Wasm, launch-contract, and binding-plan corruption
   checks.
4. Update the capability and security specifications to state that a writable
   root includes future interpretation and execution risks for files beneath it.
5. Do not merge the RFC-0155 authentication, credential, cache-exclusion, or
   cache-migration tracks. Preserve their branches only as historical work.
6. Collect startup measurements after the simplification. Propose AOT only if
   those measurements demonstrate a material remaining problem.

## Acceptance criteria

- `witchy build --target trusted-publisher` emits and runs the current packaged
  application shape with the same checked root bindings.
- The old target and `[targets.trusted-exe]` table fail loudly and name their
  replacements.
- Existing dependency-delegation, capability-isolation, confinement, trap,
  output, and exit-status tests pass under the renamed target.
- A publisher application uses the ordinary executable-cache policy; an absent
  or invalid cache entry recompiles without an interactive prompt.
- Corrupt embedded Wasm, launch-contract, binding-plan, and descriptor digests
  still fail before guest execution.
- No production cache authenticator, cache credential, or hidden cache-path
  exclusion is introduced.
- Current security documentation states that write authority over a root accepts
  future-execution influence through files under that root.
- Startup evidence reports exact commits, hardware, cold-state definition, raw
  samples, median, and dispersion before any new AOT proposal.

## Alternatives

### Authenticate every executable cache

Rejected with RFC-0155. It protects against a writer that this RFC places
outside the boundary, requires credential lifecycle and headless behavior, and
still does not neutralize other execution-triggering files under the same broad
writable root.

### Hide Witchy's cache from guest filesystem capabilities

Rejected. This changes the meaning of a delegated root, is incomplete for open
handles and native children, and creates a Witchy-specific shadow filesystem
policy. Delegate a narrower root instead.

### Embed AOT code in every publisher application

Rejected with RFC-0156 as an unmeasured mandatory solution. AOT remains available
for a later performance proposal with a complete runtime and safety design.

### Trust the complete executable as arbitrary native code

Rejected as the product description. The publisher is the first-order trusted
principal, while transitive dependencies remain guest code constrained by
Witchy's sandbox and explicit authority flow. Explicit native extensions remain
a separate trust-boundary decision.

## Drawbacks

A publisher application with broad write authority can poison executable state
used by later processes. This is accepted and must be visible in documentation
and capability review. Users seeking stronger separation must grant narrower
filesystem roots or run the application under an additional OS principal or
container boundary.

The one-cut rename breaks manifests, scripts, and documentation using
`trusted-exe`. The old term encodes the wrong trust statement, so retaining it as
an alias would preserve the ambiguity this RFC resolves.

Rejecting the completed but unmerged RFC-0155 branches discards implementation
effort. Avoiding a credential and cache-authentication subsystem whose attacker
is no longer in scope is the smaller long-term system.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->

> 2026-08-25: Implemented by `9e143bfe7` and startup evidence follow-up
> `132a4cc5a`: current targets and manifests use `trusted-publisher`, publisher
> launches use the ordinary cache-enabled runtime, and the RFC-0155 cache
> authenticator crate was removed.
