---
rfc: 0126
title: "Capabilities and explicit effects as a core language contract"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical consolidation RFC. The supported-preview capability journey is shipped; remaining work is promotion of the full effectful stdlib matrix and reconciliation of deferred extension mechanisms, not reconsideration of the capability model."
predecessors:
  - "[0002](0002-user-definable-capabilities.md), [0011](0011-capability-refinement.md), [0038](0038-grantable-user-capabilities.md) (user-defined and refined authority)"
  - "[0003](0003-network-address-scoping.md), [0020](0020-rebinding-resistant-http.md), [0057](0057-capability-policy-constructors.md) (value-level confinement)"
  - "[0009](0009-https-tls-client.md), [0060](0060-server-tls.md), [0106](0106-native-only-crypto-and-target-availability.md) (effectful stdlib transport and target availability)"
  - "[0005](0005-unforgeable-capabilities.md), [0103](0103-derived-platform-confinement.md) (compiled representation and outer confinement)"
  - "[0012](0012-file-capability.md), [0013](0013-capability-grant-documents.md), [0102](0102-portable-roots-and-the-fetch-capability.md), [0121](0121-sealed-secret-rights.md) (root capabilities and grants)"
  - "[0040](0040-grantable-caps-on-exported-entrypoints.md), [0068](0068-compiled-build-step-grants.md), [0076](0076-capability-ops-are-methods.md), [0077](0077-mockable-capabilities-for-tests.md) (entrypoints, methods, and tests)"
  - "[0014](0014-remove-capability-firewall.md), [0091](0091-browser-virtual-capabilities.md) (one explicit effect model across hosts)"
related:
  - "[0085](0085-capability-bounded-dynamic-code.md), [0086](0086-capability-gated-native-extensions.md) (deferred extension mechanisms)"
---

# RFC-0126: Capabilities and explicit effects as a core language contract

## Decision

Capabilities are part of Witchy's type and call model. Host authority enters a
program only through typed root parameters or explicitly checked host bindings.
Code exercises authority only by receiving a capability value whose rights and
scope authorize the operation.

This is Witchy's central effect system. It is not an optional security library,
an ambient permission lookup, or a convention layered on ordinary handles.

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
trusted executables preserve the same binding rule.

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
