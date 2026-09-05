# RFC-0165 implementation amendment: one memory model, checked opt boundaries

Decision date: 2026-09-04. This amendment records the user's agreed direction
after the RFC's external reviews. It supersedes conflicting migration choices,
not the independent-checker, proper-tail-call, or final-byte safety obligations.
Implementation and acceptance remain incomplete.

## One language, one value semantics

Normal Witchy and `opt` use the same observable value semantics. `opt` is a
checked interface to stronger resource and representation requirements, not a
second runtime memory model. It is useful to think of the boundary like FFI,
but both sides lower through the same SafeWIR and checked ABI.

Keep `let`, `var`, `own`, ordinary function calls, and mutation syntax such as
`xs.push(x)`. Callers do not write `xs = xs.push(x)`. Bounds are checked against
logical length, never spare capacity. Capacity growth policies need not agree
between modes or compiler versions unless the language explicitly promises it.

The baseline uses owned references with checked retain, transfer, and release.
It does not introduce a general tracing collector. Internal uniqueness remains
a useful property even if the source `unique` annotation is withdrawn.

## Scope of resource contracts

Every enforced requirement identifies whether it applies to argument
preparation, the callee's execution, returned values, or the entire caller.
Do not use an unqualified 'no copy' flag for all four meanings.

- A normal caller may prepare a value for a stricter callee, including making a
  private copy where value semantics permits. Preparation occurs before entry
  and does not constitute permission for the callee to violate its contract.
- An `opt` caller may perform that same preparation only if its own checked
  requirements permit it. Otherwise compilation rejects the call precisely.
- A promised representation or ownership precondition must be established by
  checked lowering, an appropriate runtime test and safe preparation, or a
  verified static fact. An annotation or function name never supplies it.
- A missing optimization fact retains the conservative operation or check.
  A missing intrinsic type, extent, ownership rule, or ABI contract rejects.
- Stronger analyses may eliminate copies, retains, and guards later. They may
  not silently redefine what an existing resource contract promises.

This does not allow hidden payload copies inside a promised no-copy operation.
Normal-mode adaptation is a separately identified part of the call. Existing
source restrictions remain enforced until their replacement is implemented;
there is no interval in which a guarantee is accepted and ignored.

## Initial support choices

| Surface | Initial contract |
|---|---|
| `let`, `var`, `own` | Preserve source behavior; lower to an owned reference ABI |
| Normal calling `opt` | Permit checked boundary preparation; no lifetime syntax required in normal code |
| Read-only string/collection views | Managed owner reference plus validated projection/range; no dangling interior pointer |
| Source `unique` | Withdraw from the initial supported surface; update stdlib declarations, diagnostics, docs, and fixtures explicitly |
| Internal uniqueness | Retain for safe runtime in-place mutation and later static optimization |
| Escaping mutable references | Deferred; reject rather than replace write-through semantics with copying |
| Region-backed public interfaces | Deferred; do not pretend an RC retain pins rewound region storage |
| Existing frontend lifetime checks | May remain as source restrictions; do not require a second backend borrow checker for owned read-only views |

Removing `unique` from a stdlib signature is not merely deleting a token. Each
affected API must state its replacement convention and preserve its ordinary
value behavior. Explicit no-copy/representation promises that cannot yet be
honored are unsupported, not optimization hints. Advanced-contract withdrawals
are recorded separately from accidental ordinary-language coverage failures.

## Owned ABI and lifetimes

Each memory-bearing parameter receives one owned reference token. The callee
must transfer it, store it, or release it on every normal exit. Non-tail callers
clone only when retaining their own token; tail callers transfer references
before the tail edge, with no residual caller cleanup. Proper tail calls remain
required language semantics, including with collection parameters.

Reference storage classes are explicit. RC operations never touch Bump,
Scratch, static data, or unknown storage as though an RC header existed.
Static data is immortal without header writes. A shared result must carry a
real owner/reference discipline, not just a `Shared` label.

A read-only view owns its root and a range checked at creation. Its release
targets the root, never the interior address. Mutable access must not invalidate
a live view: value-semantic copy-on-write or a checked exclusive operation is
required. A place is an ephemeral projection used while its owning reference
is live, not a storable unowned pointer.

Refcount-one authorizes mutation of that storage only. It says nothing about
the uniqueness of child objects. Nested mutation separately establishes the
required ownership along the modified path. Cloning a pointer collection
retains its children; moving slots transfers their tokens exactly once.

Destruction is layout-directed and releases initialized owned children.
Partial initialization, replaced slots, closure captures, managed views, and
mixed GC/RC containers require explicit accounting. GC does not release an RC
pointer merely because its bytes were stored in a GC field. Unsupported mixed
storage fails closed until its lifetime protocol is modeled.

The initial owned-value representation must not create strong reference cycles.
Recursive code uses callable identities rather than owning self-captures.
Self-insertion uses value-semantic copying/sharing of the prior value, not a
back-edge to mutable storage. Any future cyclic representation needs a stated
reclamation policy before admission; 'RC will handle it' is not one.

Host buffer lifetimes are part of authenticated host contracts. Same-instance
reentrancy requires pinned owners and cannot run while an unmodeled exclusive
access is active. Isolated worker instances are not same-instance reentrancy.
Host references to linear storage cannot outlive their retained owner tokens.

## IR information and optimizations

SafeWIR carries actual types, layouts, projections, logical extents versus
capacity, ownership operations, arithmetic/trap semantics, call conventions,
tail obligations, and host roles. Derived ranges, aliases, escape facts,
uniqueness, and profitability live in analysis caches keyed by revision and
program point. Absence of a cache must not change program meaning.

Initial production usability does not depend on optional optimization passes.
Runtime checks and correct in-place RC branches are the safety/performance
floor. Later transformations emit candidates for independent rechecking;
semantic-preservation confidence is a separate obligation from memory safety.
Testing/fuzzing is evidence, not a universal proof of pass composition.

## Implementation order and evidence

1. Repair recorded checker, owned-ABI, source-binding, and executable-oracle
   defects with focused positive and adversarial tests.
2. Establish central ownership insertion and layout-driven destruction, including
   pointer collections and initialized-prefix handling. Stop scattering cleanup
   guesses across source constructs.
3. Exercise owned calls and tails, managed views, normal-to-opt preparation,
   opt-to-opt rejection, and nested mutation through the same core.
4. Migrate remaining ordinary constructs by composition of those primitives.
   Retire obsolete loan/contract reconstruction paths rather than extending them.
5. Demonstrate one useful analysis-driven optimization and its near misses.
6. Complete supported-corpus admission/execution evidence and independent
   security review, then perform the coordinated production cutover and gate.

No new SafeWIR completion claim follows from the frozen legacy route. Before
cutover, canonical lowering remains the development route, with no per-program
fallback disguising its coverage gaps. After atomic cutover, production bytes
require validated SafeWIR authority and exact-byte admission exclusively.

Compiler-wide memory safety is not established by this amendment, a successful
corpus, an assurance record, or a type wrapper. Acceptance needs evidence about
the checker, backend correspondence, runtime lifetime behavior, host boundary,
and enabled transformations under the explicitly stated frontend assumptions.
