---
rfc: 0133
title: "The canonical Witchy standard-library contract"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical stdlib consolidation RFC. The core protocol and collection surfaces are shipped and generated documentation is checked. Remaining promotion work is module-by-module maturity classification, target availability, and curated installed workflows; deferred SIMD is an implementation optimization, not a missing API decision."
predecessors:
  - "[0021](0021-or-unwrap-option.md), [0044](0044-std-error-policy.md), [0048](0048-fallback-operator.md), [0054](0054-structured-errors.md) (absence and failure)"
  - "[0047](0047-one-equality.md), [0053](0053-one-rendering.md) (PartialEq/Eq and Show)"
  - "[0065](0065-sealed-type-constructors.md), [0069](0069-structured-typeinfo.md) (invariants and structural protocols)"
  - "[0074](0074-container-api-symmetry.md) (collection API coherence)"
  - "[0031](0031-simd-stdlib-hot-loops.md) (deferred implementation optimization)"
related:
  - "[0126](0126-capability-effects-contract.md) (effectful modules)"
  - "[0130](0130-generators-and-iterators.md) (Iter and FromIterator)"
  - "[0131](0131-reflection-and-comptime.md) (Reflect and derives)"
---

# RFC-0133: The canonical Witchy standard-library contract

## Decision

The standard library is part of the language experience, not a collection of
incidental compiler builtins. Public operations live in typed modules, compose
through a small protocol vocabulary, preserve value and capability semantics,
and have deliberate target availability.

Compiler intrinsics may implement an operation, but the documented module,
method, trait, constructor, and error type are the public contract.

## Canonical protocols

Each concept has one public protocol:

- `PartialEq` / `Eq` for equality;
- `PartialOrd` / `Ord` for ordering;
- `Show` for human-readable rendering and interpolation;
- `Reflect` for structural value inspection;
- `From` / `Into` for typed conversion;
- `FromIterator` for collection from lazy sequences;
- `Option` for absence; and
- `Result` for recoverable failure.

A module does not create a parallel stringly or concrete-type-only protocol
when one of these expresses the operation. Compiler fast paths must preserve
the same result as the protocol path.

## Module families

### Pure data modules

`list`, `dict`, `set`, `string`, `bytes`, `option`, `result`, `iter`, `json`,
`reflect`, `show`, `cmp`, `convert`, numeric, encoding, parsing, and related
modules are capability-free unless their signature explicitly receives an
effect capability.

Collection APIs use consistent names and result shapes. Mutation is declared
with `var`; extraction returns `Option` or `Result`; explicit discard may choose
a result-free lowering without changing used-result behavior.

### Effect modules

Filesystem, network, HTTP, server, process, environment, clock, randomness, and
secret operations receive the capability that authorizes them. Pure hashing,
signature verification, and encoding remain capability-free even when they use
a native host implementation. Authority-bearing cryptographic operations take
the relevant `Secret` or other explicit capability. These modules follow
RFC-0126's refinement, denial, target, and error rules.

### Compiler and metaprogramming modules

`meta`, reflection descriptors, syntax values, build APIs, and runtime
inspection have explicit phase and target boundaries. Compile-time-only values
cannot escape into runtime source. Native-only operations produce target
diagnostics.

## Error policy

- Normal domain and transport failure returns a structured error.
- Absence returns `Option` when no diagnostic payload is needed.
- Invalid programmer assumptions use explicit unwrap/assert/trap operations.
- Bounds, UTF-8, parsing, decoding, capability denial, and target unavailability
  retain distinct errors where callers can act differently.
- Interpreter errors and compiled traps carry equivalent source-level meaning.

## Mutation and complexity

The API documents semantic complexity independent of a particular optimizer.
Where uniqueness changes physical cost, the normal path remains copy-correct
and the opt path may require a proof. Used and discarded result paths are
documented separately when extraction or status construction changes cost.

SIMD, unboxing, specialized layouts, and host intrinsics are implementation
choices. They may strengthen measured performance but do not create alternate
stdlib semantics.

## Availability and maturity

Every public item records:

- interpreter and compiled-Wasm availability;
- browser, sandbox-host, and native-host availability;
- required capability and rights;
- normal/opt restrictions;
- stability or experimental status; and
- structured errors and traps.

Generated [`spec/stdlib.md`](../spec/stdlib.md) is the current item reference.
The book teaches curated workflows. This RFC records the stable design rules;
individual implementation details remain in source and tests.

## Acceptance

1. Generated stdlib documentation matches exported source signatures.
2. Canonical protocols cover their documented scalar, container, generic,
   nested, and user-defined matrices on both backends.
3. Collection methods agree for direct, method, function-value, used-result,
   discarded-result, unique, aliased, empty, and error cases.
4. Effect modules expose exact capability and target requirements.
5. Every documented structured error has positive and negative executable
   evidence; trap behavior agrees across backends.
6. No user-facing behavior depends on an undocumented compiler-private helper.
7. Each module family has an explicit product maturity rather than inheriting a
   blanket status from the whole standard library.
8. Installed examples cover pure data, iteration, reflection, one bounded
   effect, and one failure-handling workflow.
