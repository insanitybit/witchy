---
rfc: 0126
title: "Capabilities and explicit effects as a core language contract"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical consolidation RFC. The supported-preview capability journey is shipped; remaining work is promotion of the full effectful stdlib matrix and reconciliation of deferred extension mechanisms, not reconsideration of the capability model."
predecessors:
  - "[0002](retired-rfcs.md), [0011](retired-rfcs.md), [0038](retired-rfcs.md) (user-defined and refined authority)"
  - "[0003](retired-rfcs.md), [0020](retired-rfcs.md), [0057](retired-rfcs.md) (value-level confinement)"
  - "[0009](retired-rfcs.md), [0060](retired-rfcs.md), [0106](retired-rfcs.md) (effectful stdlib transport and target availability)"
  - "[0005](retired-rfcs.md), [0103](retired-rfcs.md) (compiled representation and outer confinement)"
  - "[0012](retired-rfcs.md), [0013](retired-rfcs.md), [0102](retired-rfcs.md), [0121](retired-rfcs.md) (root capabilities and grants)"
  - "[0040](retired-rfcs.md), [0068](retired-rfcs.md), [0076](retired-rfcs.md), [0077](retired-rfcs.md) (entrypoints, methods, and tests)"
  - "[0014](retired-rfcs.md), [0091](retired-rfcs.md) (one explicit effect model across hosts)"
related:
  - "[0085](retired-rfcs.md), [0086](retired-rfcs.md) (deferred extension mechanisms)"
---

# RFC-0126: Capabilities and explicit effects as a core language contract

## Decision

Capabilities are part of Witchy's type and call model. Host authority enters a
program only through typed root parameters or explicitly checked host bindings.
Code exercises authority only through values it receives: a capability whose
rights and scope authorize an operation, or an opaque callable delegating a
specific authorized behavior. Receiving that callable does not expose its
captured root capabilities or imply purity. [RFC-0167 section 7](0167-witchy-language-contract.md#7-resources-effects-and-suspension)
owns the callable and effect distinction; the security obligations here remain.

This is Witchy's central effect system. It is not an optional security library,
an ambient permission lookup, or a convention layered on ordinary handles.

## Current delivery boundary

Capabilities are a core part of the language, not a later extension. RFC-0167
owns their source-level call and effect semantics. This RFC owns the security
contract that those semantics depend on: authenticated host roots, monotone
rights narrowing, checked footprints and grants, and confinement at launch and
host calls. Current work must preserve those boundaries for normal-mode
programs and admitted artifacts. A passing source type check alone does not
establish that a host grant or executable artifact is authentic.

The supported-preview workflow is narrower than a complete audit of every
effectful standard-library module or host. Promote additional modules only
with evidence at their actual host boundary. Deferred dynamic code, native
extensions, and symbolic grant proposals are not required for this delivery.

## Canonical model

### Authority is not data

Root capabilities are host-authenticated references. Guest integers, strings,
JSON, reflection, deserialization, and `Dynamic` values cannot manufacture
authority. Capability-bearing values retain their authenticated representation
through every supported aggregate and call boundary or fail closed before
execution.

### Effects are explicit inputs

`Console`, `Dir`, `File`, `Net`, `Fetch`, `Secret`, `Clock`, `Rand`, `Env`, and
other root authorities appear in function signatures. Pure code has no ambient
route to those operations. Standard-library effect operations are methods on
the capability that authorizes them.

### Refinement is monotone

A capability may be narrowed by path, host, protocol, right, secret operation,
or library-defined policy. Refinement cannot widen the underlying authority.
Derived capabilities carry both authenticated authority and checked policy
state.

### Grants bind programs to hosts

Source footprints state what a root entrypoint may demand. Grant documents and
host launch arguments state what a particular run receives. Launch succeeds
only when the host grant covers the checked footprint. Precompiled Wasm and
trusted executables preserve the same binding rule. Portable precompiled Wasm
carries its closed source; the host re-admits that source and requires the
recompiled module to equal the shipped bytes before it checks any grant.
Detached module bytes carry no admission and are refused.

### Tests are not the sandbox

In-language unit tests may use deterministic test doubles. Integration tests
may receive explicit real grants. The VM and host binding layer remain the
security boundary; a mock object does not claim host confinement.

## Standard-library contract

Effectful modules follow four rules:

1. the authorizing capability is visible in the call;
2. narrowing happens before the operation and is reflected in the value;
3. errors distinguish denied authority from transport or domain failure; and
4. native-only or browser-only operations fail at target checking rather than
   disappearing at runtime.

### Rule 3 for filesystem and environment capabilities

A `Dir`, `File`, or `Env` operation that can fail at run time returns
`Result(T, CapError)`. `CapError` is one preluded type:

```witchy-static
type CapError:
    Denied(String)
    NotFound(String)
    NotAFile(String)
    AlreadyExists(String)
    Io(String)
```

The host classifies each failure once:

- a path that escapes the capability, an absolute path, an alias out of the
  root, an entry-policy refusal, or a host permission refusal is `Denied`;
- a missing entry is `NotFound`;
- a directory where a file is expected is `NotAFile`;
- an existing entry that an exclusive create refuses is `AlreadyExists`; and
- any other failure, including text that is not UTF-8, is `Io`.

`env.get_env` returns `Result(Option(String), CapError)`: an unset variable is
`Ok(None)` and a name outside the grant's allow-list is `Err(Denied(..))`.

No answer is an existence oracle. The program can branch on the variant, so a
variant never depends on what the capability cannot observe:

- an absolute or `..` path, or a name the entry policy refuses, is `Denied`
  before any filesystem access, whether or not anything exists there;
- a read through a symlink that leaves the root is `Denied` whether or not its
  target exists; and
- under an entry policy, an entry the policy cannot see is `NotFound`, exactly
  like a missing name, as `dir.exists` answers `false`. Such entries are
  aliases, paths through an alias or a non-directory, case-folded spellings,
  and directories a `files()` handle cannot open. A read under a policy never
  follows a symlink leaf, even one swapped in after the policy check.

The rule covers only failures the program cannot rule out statically. A missing
right is still a check-time error. A forged or wrong-kind host token, an
invalid guest slice, a result too large for the guest ABI, or other ABI misuse
still traps. There are no `try_` variants: one operation, one result.

A `CapError` message is guest-visible data, so it names only what the
capability already reveals. A message names the path as the guest supplied it
and gives a fixed reason. It never contains a host-resolved, canonicalized, or
absolute host path, raw operating-system error text, or another allow-listed
name. Host diagnostics for traps may keep full detail.

`dir.read` implements this rule end to end in admitted Wasm. The remaining
`Dir`, `File`, and `Env` operations migrate one slice at a time.

The cryptographic, networking, filesystem, process, server, environment, and
secret modules are retained features. Pure hashing, verification, and encoding
need no authority merely because their implementation is host-native. Secret
operations and other authority-bearing cryptography receive the relevant
capability. Platform availability may differ, but it is typed and documented.

## Extensions

Capability-bounded dynamic code and capability-gated native extensions remain
deferred under RFC-0085 and RFC-0086. They are extension mechanisms over this
contract, not missing pieces of the core capability model. Any revival must
preserve explicit authority, target availability, footprint accounting,
determinism boundaries, and fail-closed linking.

## Acceptance

1. Root authority is unforgeable on every supported execution path.
2. Footprints, grants, precompiled modules, and trusted executables agree on the
   same rights and scope model.
3. User-defined capability refinement is monotone and footprint-transparent.
4. Direct and transitive capability-bearing aggregates either preserve the
   authenticated host reference or receive a check-time rejection.
5. Effectful standard-library examples cover success, denial, malformed input,
   and unavailable-target behavior.
6. `witchy caps`, `caps-diff`, `grants-check`, and `sandbox` remain executable
   evidence for the public contract.
7. The product ledger separates implemented effect modules from independently
   audited or hosted-service claims.
