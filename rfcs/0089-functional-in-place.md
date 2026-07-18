---
rfc: 0089
title: "Functional-in-place state kernels"
status: implemented
created: 2026-07-14
updated: 2026-07-15
tracking: initial scalar-record kernel shipped; recursive ADT reuse remains additive future work. The checked source contract (criterion 5) is pinned per rejection class in [tests/misc/rfc0089_fip_contract.rs](../tests/misc/rfc0089_fip_contract.rs) — non-tail recursion, replacement exit, helper call, owner escape, loops, heap field, heap auxiliary — plus non-candidate shapes (non-recursive and Result-wrapped owners) and the canonical kernel's both-backend parity at 50,000 transitions
related:
  - "0016 (reference-counted memory - supplies the allocation floor and counters)"
  - "0026 (unique qualifier - supplies the ownership surface)"
  - "0029 (performance-tier contract - makes a missed proof an opt-mode error)"
  - "0033 (place-based uniqueness - carries ownership through user records)"
  - "0090 (proper tail calls - supplies the constant-stack lowering)"
---

# RFC-0089: Functional-in-place state kernels

The source-contract checks are collected in
[`tests/misc/rfc0089_fip_contract.rs`](../tests/misc/rfc0089_fip_contract.rs),
with end-to-end counters and parity exercised by the same test harness.

## Summary

Witchy supports a checked functional-in-place (FIP) kernel in `mode opt` without
adding a keyword, attribute, or second function syntax. A direct self-recursive
function opts into the contract when it has exactly one `own unique T`
parameter, returns `unique T` of the same type, and threads that owner through
its recursive tail edge.

The initial kernel class is deliberately useful and auditable: scalar state
machine work over one uniquely owned record. It guarantees that recursive depth
adds:

- no allocator calls;
- no deallocator or free-list-reuse calls;
- no arena rewinds; and
- no control-stack growth.

The ordinary source remains functional at its boundary: the caller consumes a
value and receives a value. The compiler may update the uniquely owned record in
place and lowers the complete value-plus-ownership-token tail result to one
loop.

> 2026-07-15: Revived from `deferred` with a deliberately narrower first rung.
> The research note's original revival gates targeted recursive ADTs and
> required a non-toy Witchy workload, shared-input rejection, stack high-water
> measurement, and an end-to-end benchmark. This RFC does not claim those gates
> were met. It replaces them for the scalar-record kernel with a checked source
> contract, ownership-envelope lowering, exact compiled operation counters, and
> structural constant-stack verification. Recursive-ADT FIP retains the
> original evidence burden as additive future work.

## Source contract

A function is an FIP candidate when all of these hold:

1. its containing module declares `mode opt`;
2. it has exactly one parameter of the form `own x: unique T`, where `T` is a
   record whose stored fields are scalar;
3. its result is `unique T` for that same unqualified `T`; and
4. every auxiliary parameter is scalar; and
5. its body contains a direct recursive call to itself.

Non-recursive consume-and-return helpers do not opt in merely because their
signatures have the same ownership shape.

No special marker is needed. The ownership signature states the semantic
contract, direct recursion identifies the kernel, and `mode opt` already means
that a missed performance proof is an error rather than a silent fallback.

```witchy
mode opt

type Cursor:
    offset: Int
    checksum: Int

fn scan(own cursor: unique Cursor, remaining: Int) -> unique Cursor:
    if remaining == 0:
        return cursor
    cursor.checksum = (cursor.checksum * 33 + remaining) % 65521
    cursor.offset = cursor.offset + 1
    scan(cursor, remaining - 1)
```

Every reachable non-recursive exit returns the owner directly. In this initial
contract, base cases use explicit returns and the one recursive edge is the
function's final expression. It passes the owner directly in its original
parameter slot. Arguments still evaluate left to right and parameter rebinding
is simultaneous. Tail recursion nested in `if`, `match`, a block, or explicit
`return` remains semantically proper under RFC-0090, but its multi-result
ownership envelope is not yet admitted as FIP.

## Checked kernel body

The implemented kernel class admits:

- scalar literals, locals, arithmetic, comparisons, boolean and bit operators;
- reads of fields rooted at the owned record;
- assignments to fields of that record;
- `if`, `match`, and lexical blocks for scalar work and base-case exits; and
- explicit or fallthrough returns of the owner.

It rejects with an actionable `mode opt` diagnostic:

- non-final or non-tail recursion, or an edge that does not forward the owner;
- an exit that returns a replacement value instead of the owner;
- copying, aliasing, or otherwise escaping the owner;
- aggregate or closure construction inside the kernel;
- calls other than the direct recursive edge;
- `?`, async suspension, generators, loops, ranges, indexed access, regions,
  and first-class calls; and
- heap-valued record fields or auxiliary parameters.

This is intentionally stricter than semantic validity. Normal Witchy remains
copy-correct; this signature in `mode opt` asks for a stronger resource theorem.

## Lowering

RFC-0033's compiled `own` ABI passes an aggregate value with a hidden ownership
token and returns both. Qualifiers are representation-transparent, so
`own unique T` participates in the same ABI as `own T`.

[RFC-0090](0090-proper-tail-calls.md)'s ordinary scalar tail pass is extended to recognize the canonical
two-result envelope:

```text
(value: T, ownership_token: i32)
```

For a qualifying self tail call the pass stages every explicit argument and the
token, clears and rebinds the current activation's locals, then branches to one
loop header. It does not leave a recursive call, caller-side write-back, drop,
conversion, or token repair after the branch. Arbitrary multi-result calls are
not rewritten; only the ownership envelope with exact forwarding qualifies.

Fresh aggregate arguments enter the ABI with their known initial ownership
token. A forwarded `own` parameter carries its current token instead of being
silently downgraded to unknown ownership.

## Verification

The interpreter is the value oracle, not the resource oracle. Its representation
and cloning strategy intentionally differ from compiled Wasm.

Compiled modules export five monotonic counters through `witchy stats`:

| Counter | Event |
|---|---|
| `rc_alloc_calls` | entry to the RC allocator |
| `bump_alloc_calls` | entry to the arena bump allocator |
| `rc_reuse_calls` | a successful free-list reuse |
| `rc_free_calls` | entry to the RC free helper, including guarded no-ops |
| `region_rewind_calls` | a generated region or loop-watermark heap rewind |

An FIP resource test compares a shallow and a deep execution with identical
fixed setup. All five counts must be equal. The deep representative state
kernel is also required to complete at a depth that would overflow ordinary
recursive control stack. WIR tests independently require the ownership envelope
to become one loop with no recursive `CallStoreMulti` edge.

The checked-in oracle compares 8 and 50,000 transitions. Both perform exactly
four fixed setup allocations and zero reuse, free, or rewind operations.

## Diagnostics

CLI, browser compilation, statistics runs, tests, and the LSP all consume the
same module-level FIP analysis. Diagnostics name the function and source line,
state the failed proof, and restate the repair rule: keep recursion in tail
position, forward the owner, mutate only its fields, and return it on every
exit.

## Non-goals

This RFC does not claim FIP for:

- mutually recursive ownership envelopes;
- recursive ADT constructor reuse, tree zippers, or size-changing transforms;
- trait, generic, or indirect calls inside the kernel;
- borrowed collection traversal or indexed parser input;
- bounded allocation budgets greater than zero; or
- arbitrary functions returning `unique` values.

Those are additive extensions. They require their own typed-WIR proof and
resource tests; they do not weaken this kernel's zero-operation guarantee.

## Alternatives

- **Add `fip fn` or an attribute.** Rejected. The ownership signature and
  `mode opt` already carry the necessary intent; another syntax would bifurcate
  functions without adding semantic information. Structural activation is
  deliberate only inside `mode opt`, and `own unique` is already an explicit
  consume-and-return performance contract rather than an ordinary helper shape.
- **Infer the optimization in every mode.** The loop optimization may still
  apply wherever sound, but only `mode opt` turns the complete resource proof
  into a source contract.
- **Promise FIP for every `unique` function.** Rejected. Uniqueness alone does
  not prove owner forwarding, allocation freedom, or bounded recursion.
- **Use only timing benchmarks.** Rejected. Timings cannot prove absence of a
  rare allocation or stack-growth path; operation counters and WIR structure can.
- **Use interpreter allocation counts.** Rejected because interpreter resource
  behavior is not the compiled representation.

## Drawbacks

- The accepted kernel class is narrower than all semantically valid recursive
  state transformations.
- Adding an unrelated helper call can invalidate the contract until summaries
  become proof-carrying across calls.
- The structural opt-in means changing a consume-and-return helper from
  iterative to recursive can activate stricter `mode opt` checking.
- Resource counters add a small fixed amount of instrumentation to compiled
  allocator and reclamation helpers.

## Prior art

[FP2: Fully in-Place Functional Programming](../external-refs/fip-fully-in-place-2023/)
provides the stronger formal destination: linear functional programs with no
allocation/deallocation and constant stack under stated conditions. This RFC
ships a conservative Witchy subset whose theorem can be checked against the
actual Wasm ABI today. [Perceus](../external-refs/perceus-2021/) and Lean 4
provide the dynamic reuse baseline; Witchy's `mode opt` contract differs by
rejecting a missed static proof instead of silently relying on runtime reuse.

## Acceptance criteria

1. `own unique T -> unique T` uses the ownership ABI through qualifiers.
2. A forwarded owner preserves its hidden ownership token.
3. The complete two-result self-tail envelope lowers to one loop.
4. The canonical state kernel succeeds at 50,000 transitions.
5. Non-tail recursion, replacement exits, owner escape, allocation, and effects
   fail with source-located `mode opt` diagnostics.
6. CLI/browser enforcement and LSP diagnostics share the same analysis.
7. Alloc, bump, reuse, free, and rewind counters are exported by `witchy stats`.
8. Shallow and deep runs have identical operation counts and value-correct
   outputs.
9. The spec, book, and runnable `opt_mode` example document the contract.
