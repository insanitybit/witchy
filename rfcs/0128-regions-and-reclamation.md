---
rfc: 0128
title: "Regions and predictable memory reclamation"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical region RFC. Lexical regions, Wasm watermark reclamation, copy-out, counters, parity, and examples are shipped. Promotion to supported preview requires an installed example and an explicit limitation review; destination inference remains measurement-gated optimization work."
predecessors:
  - "[0016](0016-reference-counted-memory.md), [0035](0035-completing-the-rc-floor.md) (general reclamation floor)"
  - "[0024](0024-unified-facts-lattice.md), [0051](0051-memory-safety-invariants.md) (escape facts and memory invariants)"
  - "[regions](regions.md) (original phased region design and implementation record)"
related:
  - "[0127](0127-ownership-and-opt-mode.md) (ownership and access facts)"
  - "[0129](0129-concurrency-tasks-and-channels.md) (task-frame reclamation)"
---

# RFC-0128: Regions and predictable memory reclamation

> **2026-08-21 stability note:** implementation evidence establishes safe value
> semantics and backend parity, but not that explicit syntax is necessary beside
> ownership annotations and inferred reclamation. The `region:` keyword is
> therefore unstable: the compiler warns on use, and the syntax and performance
> contract may change or be removed. This dated note supersedes the permanence
> claim below while preserving the accepted RFC as historical design context.

## Decision

`region:` is a permanent Witchy feature. It gives a lexical scope for temporary
allocation and a predictable bulk-reclamation point without changing program
values, capability behavior, or normal-mode ergonomics.

The programmer selects a lifetime boundary, not an allocator implementation.
The interpreter may execute a region as an ordinary block. Compiled Wasm may
use a watermark, destination lane, stack allocation, or another proven strategy.
Skipping a region optimization is always semantically valid.

## Source model

```witchy
let summary = region:
    let parsed = parse_huge_input(text)
    summarize(parsed)
```

The block's final value escapes. Region-local temporary storage does not. An
optional result ascription states the copy-out shape when inference is
insufficient:

```witchy
let rows = region -> List(Row):
    build_rows(input)
```

## Semantics

1. A region is an expression and returns its tail value.
2. Parent-owned subvalues may pass through without copying.
3. Region-born parts of the result are copied or constructed into parent
   storage before reclamation.
4. Assigning a region-born pointer into an outer ordinary variable is rejected;
   scalar assignments and checked host-boundary copies remain valid.
5. `yield` and suspension cannot carry region storage beyond the scope.
6. Capability handles remain host-owned and may be used normally inside a
   region; payloads crossing a host call use the ordinary checked copy boundary.
7. A failure to prove an optimized strategy falls back to correct block
   execution unless source explicitly requested a stronger opt-mode contract.

## Reclamation stack

Regions coexist with, rather than replace, Witchy's other memory mechanisms:

- arena allocation handles short whole-run lifetimes cheaply;
- lexical regions reclaim known temporary phases in bulk;
- loop and task watermarks reclaim proven confined iterations;
- uniqueness and destination passing avoid allocations entirely;
- the RC floor reclaims long-lived objects that leave a lexical allocation
  regime and later become unreachable.

Each mechanism consumes the same ownership, escape, and layout facts. No
container or standard-library method receives a private memory model.

## Performance contract

Region value semantics are unconditional. Reclamation and copy avoidance are
measured properties of a particular compiled path.

`witchy stats` exposes region entry, rewind, and copy-out facts. A useful region
should show bounded memory or reduced allocation on its target workload. A
region that copies most of its live result remains correct and may be a poor
optimization choice.

Destination inference is retained as a future optimization only when measured
copy-out volume justifies its allocator and code-generation cost. It does not
change the source contract.

## Interaction with references and concurrency

- A reference whose owner is region-born cannot escape the region.
- A parent-owned reference may be used inside the region while its ordinary loan
  remains valid.
- An async task, generator, channel message, closure, or `Dynamic` value must
  own any region-born payload that outlives the region.
- A compiler-generated task or generator frame may use internal region-like
  reclamation when its captured values satisfy the same escape rules.

## Acceptance

1. Parser, formatter, checker, book, and reference spec agree on `region:` and
   result ascription.
2. Interpreter and Wasm agree for scalar, string, list, dictionary, record,
   recursive ADT, nested-region, and loop interactions.
3. Invalid outer pointer assignment, escaping references, suspension, and
   `yield` fail before backend execution.
4. A soak fixture proves bounded compiled memory for repeated large temporary
   allocations.
5. Counters prove parent-value passthrough and report region-born copy-out.
6. A clean installed binary runs one curated region workflow and documents when
   the construct helps.
7. Promotion promises the conservative copy-out semantics, not unimplemented
   destination inference.
