---
rfc: 0090
title: "Guaranteed proper tail calls and callable ABI convergence"
status: implemented
created: 2026-07-14
implemented: 2026-07-15
related:
  - "0005 (unforgeable capabilities - callable representations must retain capability kinds)"
  - "0029 (performance-tier contract - opt mode may require resource proofs)"
  - "0059 (state-machine async - suspension is already explicit control state)"
  - "0083 (opt-mode lifetimes - loans must end or transfer across a tail edge)"
  - "0087 (uniform var write-back - move-out is part of the callable ABI)"
  - "0089 (fully in-place functional kernels - requires constant control stack)"
tracking: stages 1-3 are implemented. The typed closure ABI landed in commit cb911f8a; tests/rfc0090_indirect_tail.rs and structural WIR tests cover exact scalar and reference-bearing indirect signatures, simultaneous rebind, out-of-component exits, and near-tail residual work. Criterion 7 is complete in tests/rfc0090_var_tail_envelope.rs: one and multiple RFC-0087 write-backs, capacity tokens, and explicit returns forward through constant-stack self loops or ABI-compatible mutual dispatchers on both backends, while nested-place reconstruction remains non-tail. Criterion 9 is complete in tests/rfc0090_async_loan_tail.rs: generated async segment cycles use the portable loop, suspension remains resumable state, ended loans permit a tail edge, live post-call loans prevent one, and returned-view obligations transfer through mutual tail edges. The spec and book state the bounded-control-stack guarantee and its allocation limits.
---

# RFC-0090: Guaranteed proper tail calls

## Summary

Witchy guarantees that a proper tail call consumes constant control stack. The
guarantee is part of language semantics in every mode, not an optimizer promise,
an implementation accident, or special call syntax.

A call is proper when its result and callable-ABI results can become the current
function's results directly, with no caller computation, write-back, drop,
borrow termination, representation conversion, or error inspection remaining.
The compiler diagnoses an unlowerable proper tail call instead of silently
emitting a stack-growing call.

The implementation does not depend on WebAssembly's tail-call proposal. Direct
self recursion lowers to parameter staging and a loop. Mutually recursive direct
functions lower to one state machine per compatible call-graph component.
Indirect calls use a typed closure-table dispatcher until the runtime's locked-down
Wasm feature set can admit an equivalent native instruction. Native tail calls may
replace these lowerings only under differential and resource tests.

## Motivation

Recursion is ordinary Witchy style today, but its resource behavior is backend
dependent. The interpreter recursively enters Rust frames and guards a finite
depth. The compiled backend emits ordinary Wasm `call`, while the production
engine explicitly disables the tail-call proposal. A tail-recursive program can
therefore fail only because of input size even though its live language state is
constant.

That is already a language problem and becomes a foundation blocker for
RFC-0089. A certified fully in-place kernel cannot promise constant stack if
tail calls remain best-effort. Async state machines, convention-bearing function
values, lifetimes, and capability references also need one explicit answer to
what crosses a call edge and what remains in the caller.

## Semantics

### Proper tail position

The normative rule is continuation-based: an expression is in tail position
when its value is returned by the current callable and evaluating it leaves no
observable work in that callable.

This includes:

- the final expression of a function or closure;
- the operand of an explicit `return`;
- the selected branch tail of a tail-position `if`;
- every selected arm body of a tail-position `match`;
- the final expression of a tail-position block; and
- the fallback of tail-position `??` after the left operand has selected it.

It does not include operands of arithmetic, construction, interpolation, `?`,
guards, loop conditions, call arguments, or the left side of `??`. Those forms
inspect, combine, or conditionally transform the callee's result.

### Callable ABI compatibility

Source return-type compatibility is necessary but not sufficient. A proper tail
edge must be able to forward the complete callable result envelope unchanged:

1. the declared result;
2. every RFC-0087 `var` move-out result;
3. ownership tokens used by the compiled representation;
4. capability/reference kinds from RFC-0005; and
5. lifetime/loan obligations from RFC-0083.

A call such as `f(var xs)` normally has caller-side place reconstruction and is
therefore not proper even when textually final. It becomes proper only when the
compiler proves that the entire write-back envelope is forwarded to identical
caller results without reconstruction. The same rule applies to ownership-token
and loan cleanup. This is one semantic test, not a list of method exceptions.

### Resource guarantee

An unbounded sequence of proper tail edges uses constant control stack. The
guarantee says nothing by itself about heap allocation, container copying, total
time, or termination. RFC-0089 layers stronger allocation and reuse proofs on
top of this control-stack floor.

Runtime errors still report the currently executing logical function. Tail
lowering may remove physical frames, so a future stack-trace facility must expose
logical tail transitions explicitly rather than fabricate retained frames.

### Modes

Normal and `opt` mode have identical calls and results. Normal mode guarantees
bounded control stack for proper calls. `opt` may additionally reject a kernel
whose declared resource contract cannot prove bounded allocation, reuse, or loan
state; it may not weaken proper-tail-call behavior.

## Lowering

### Stage 1: direct self calls

For a compatible single-result function, lower every self-tail edge to:

1. evaluate arguments left-to-right into fresh typed staging locals;
2. rebind all parameters from those locals simultaneously; and
3. branch to the function-body loop header.

Non-recursive exits return directly. Staging is mandatory: `f(b, a)` must swap,
not assign `a = b` and then read the new `a` for the second argument.

### Stage 2: direct recursive components

Compute strongly connected components from proper direct edges. A component
with more than one callable lowers to a dispatcher loop with a typed state tag
and disjoint parameter banks. Each edge stages the destination arguments, sets
the destination state, and branches to the dispatcher header. Public and
non-tail entry wrappers preserve the original callable ABIs.

The portable implementation uses one WIR dispatcher per component. Its parameter
banks retain each member's exact scalar or reference kinds. After staging the
arguments, every transition clears the departing bank (releasing reference
roots), rebinds the destination parameters, clears its staging temporaries, and
resets its local bank before entering the body. The state machine therefore has
ordinary fresh-call semantics.

Only ABI-compatible edges join a dispatcher. An incompatible edge remains an
ordinary non-tail call because forwarding would leave real work in the caller.

### Stage 3: generics, traits, and closures

Monomorphized direct calls participate after specialization. Devirtualized trait
and closure calls use the direct machinery. For a remaining indirect proper edge,
the compiler adds every exact-signature target in the finite closure table to the
recursive graph. A recursive component's dispatcher stages the closure environment
and arguments, evaluates the table index once, and selects the destination's typed
bank. A table target outside the recursive component remains an ordinary indirect
exit because it cannot contribute an unbounded cycle back into that component.
When scalar members use different Wasm result kinds, the dispatcher carries the
component result in the closure ABI's i64 slot and each public entry wrapper
recovers its declared kind. This removes the representation conversion from the
recursive continuation without erasing capability or GC references.

The interpreter carries the selected closure value and its captured environment
through the same callable-boundary loop used for named functions. It does not
recurse through a host frame to model dynamic dispatch.

No capability or GC reference may be boxed into an integer slot to fit the
trampoline. RFC-0005 representation kinds remain authoritative.

### Stage 4: native backend refinement

The compiler may emit Wasm `return_call`/`return_call_indirect` only after the
runtime security configuration permits those features and parity/resource gates
show identical behavior. The loop, component, and trampoline lowerings remain
the portable reference and browser fallback.

## Async, generators, and lifetimes

Async and generator source functions are lowered before this pass. Their segment
or step functions participate as ordinary generated callables; a suspension is
not a tail call because the executor retains resumable state.

An RFC-0083 loan must end before a tail edge or transfer in the callee's declared
result obligations. A hidden caller cleanup is residual work and makes the edge
non-proper. Normal-mode callers use the same checked obligation even though they
do not spell lifetime parameters.

## Diagnostics

The compiler must distinguish:

- a proper call lowered with the constant-stack guarantee;
- a textually final call that is not proper because it retains write-back,
  conversion, drop, error-inspection, or loan work; and
- an internal lowering failure for a proper call, which is a hard compile error.

Users do not annotate tail calls and there is no `recur`, `become`, or call-site
sigil. Compiler tooling may expose the classification in IR dumps and opt-mode
proof reports.

## Acceptance criteria

1. Interpreter and compiled backend execute at least five million direct
   self-tail transitions with constant control stack.
2. Argument evaluation is left-to-right and parameter rebinding is simultaneous
   for scalar, aggregate, capability-reference, generic, and closure values.
3. Tail positions cover final expressions, explicit return, nested blocks,
   `if`, `match`, and `??`; near-tail negative tests cover operators, `?`, guards,
   constructors, and post-call drops.
4. Non-tail recursion retains a graceful depth/stack failure rather than being
   misclassified.
5. Direct mutually recursive SCCs run at the same depth and preserve public
   callable ABIs.
6. Specialized generic, trait-dispatched, devirtualized closure, and genuinely
   indirect proper calls pass both-backend differential tests.
7. RFC-0087 multi-result calls are lowered only when their complete write-back
   envelope forwards unchanged; nested-place reconstruction remains non-tail.
8. RFC-0005 reference kinds cross no integer-slot erasure, and the portable
   lowering works while `wasm_tail_call(false)` remains configured.
9. Async segment functions and RFC-0083 loan transfer/end cases have explicit
   positive and negative tests.
10. A resource counter or structural IR assertion proves that the portable
    lowering contains no recursive backend call on guaranteed edges.
11. The spec and book teach recursion as a bounded-stack control abstraction,
    while documenting that heap/allocation guarantees require RFC-0089.

## Alternatives

### Special tail-call syntax

Rejected. Whether a continuation is empty is a semantic property the compiler
already knows. Requiring users to bifurcate ordinary calls would make refactors
change syntax without changing meaning and would let unannotated tail calls
silently lose the guarantee.

### Optimizer-only tail-call elimination

Rejected. An optimization miss would become an input-dependent runtime failure.
The transformation is mandatory and tested with optimizations disabled.

### Require the Wasm tail-call proposal

Rejected as the foundation. It is useful when admitted by the runtime security
policy, but Witchy's interpreter, browser targets, and locked-down engine must
share the language guarantee.

### Tail recursion only

Rejected as the endpoint. Self recursion is the first lowering slice, not the
language boundary; mutual and indirect proper calls are equally observable.

## Drawbacks

- SCC dispatchers and indirect table selection add compiler and debug-info work.
- Convention-bearing multi-result ABIs make fewer textually final calls proper
  than a single-result language.
- Logical stack traces need an explicit tail-transition representation.
- Portable dispatch may be slower than native Wasm tail calls until stage 4.
