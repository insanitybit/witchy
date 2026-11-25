# Typed named specification fragments

Library authors can name a pure expression and use it in supported function
`requires` and `ensures` clauses. The actual Vec library defines:

```text
predicate readable<T>(buffer: &Buffer<T>, index: U64):
    index < memory.capacity(buffer) && memory.initialized(buffer, index)

predicate within(value: U64, limit: U64):
    value <= limit
```

The actual unchecked-read wrapper uses `requires readable(buffer, index)`.
Bounds and initialization remain proof obligations at the actual unsafe operation
and caller. `Vec.get` retains its executable logical-length, physical-capacity and
occupancy guards. The spare-push post uses `within(output.length, output.capacity)`;
the declaration does not silently establish a Vec invariant or validate headers.

## Resolution and meaning

One common typed expansion pass runs before existing proof-model routing.
Formals have declaration/parameter identities; actual paths are substituted
without capturing function parameters, fields or result-arm binders. Generic
arguments are inferred from typed actuals. Each body is one Boolean expression;
supported forms include Boolean logic, U64 arithmetic/comparisons, typed paths,
supported equality and existing memory observations. Effects are not predicates.

Every selected definition is checked, including unused definitions. A well-typed
body of `false` is valid; a reachable claim that it holds still requires a proof.
Models receive ordinary expanded expressions, not predicate assumptions or new
Lean axioms. Unsupported contracted function shapes remain unsupported.

`old` retains function-entry snapshot meaning where the existing model supports
it, including under mutation. It does not denote the previous iteration.
Unsupported nested snapshots and contexts refuse. Fragments grant no runtime
ownership, borrow or capability authority.

## Bounds and exclusions

Definitions can call other nonrecursive fragments. Cycles, unknown calls and
effects refuse. Resolution has a separate 64-level dependency bound as well as
expression-depth and node limits. Definitions allow at most eight generic and
32 value parameters. The existing selected declaration limit includes
predicate declarations before expansion.

Before materialization, the common pass conservatively charges repeated actual
substitutions, arguments, generated syntax scaffolding and text against cumulative
4,096-node and 65,536-byte normalization budgets. Existing proof-model,
generated-statement and checker-request limits remain independent and unchanged.

Type invariants and method preconditions and postconditions accept named predicates.
A public predicate can describe its own module's sealed fields and shared projection
methods. Imported code can use the predicate. Each argument is checked in its
caller's scope. Every method mentioned by a predicate must have a checked
projection body, including methods in unused predicates.

A guarded clause observes a sum in the current or entry state. Its binder is
local to the matching arm. Other variants satisfy the clause vacuously. A
predicate can use an exhaustive match to assign a truth value to every variant.
Multiple lines in an arm are conjoined.

```rust
invariant when Some(prev) = cell.read().prev:
    same_target(prev.cell.read().next, cell.pointer())
```

In the linked-list library, `same_target` compares address and allocation
identity. `member_of` separately names the proof that a returned loan belongs
to the chosen list. Both are Witchy definitions. `old(pointer)` observes the
same entry state after substitution, including when a path crosses a shared
projection or a matched payload.

Automatic discharge is the default for function contracts. An unchecked
operation, a protected mutation of a sealed invariant, or an explicit
`unreachable` in a `nopanic` helper can require a proof even when the function
has no written clauses. The checker resolves the operation and operand types
and checks the body under the same rules as an explicit `ensures true`.
Functions without obligations continue down the ordinary typed path.
Explicit function proofs currently accept one `auto` step. Lemmas retain
their existing proof syntax. Named predicates remain specification-only.
Named loop invariants and general recursive predicates remain outside this
fragment.

`memory.frame_unchanged` is refused everywhere: it rendered only reflexive frame
equality. No clause provides a frame or physical-isolation theorem. See [the trust boundary](proof-architecture.md).

## Focused evidence

Source tests compare expanded goals and prover text with handwritten inline
conjunctions of the same shape. The earlier two separate `requires` clauses are
a logical/behavioral baseline, not a byte-equality promise for grouping. Naming
alone is not a proof-performance improvement.

Checks exercise the actual Vec library and Wasmtime, result-name capture,
entry-state Empty followed by initialization, false uses, invalid callers,
missing initialization, unused type errors, recursion and expansion limits.
Refusals preserve valid output. A proof-free program with an unused false
fragment executes with zero Lean invocations. An invalid explicit operator
binding still refuses; that validation is not bypassed for the proof-free case.

Translation/completeness, source ownership checking and physical memory/runtime
mapping remain trusted. This is not a whole Vec invariant, termination proof,
owning-element storage or physical-isolation theorem.
