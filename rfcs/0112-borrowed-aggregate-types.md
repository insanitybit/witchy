---
rfc: 0112
title: "Borrowed aggregate types and projection-aware loans"
status: accepted
created: 2026-08-01
updated: 2026-08-08
tracking: "Design accepted; no open questions (non-goals fixed: no mutable/shared refs, no pointer identity, no loan across await/yield initially). Adds lifetime parameters to nominal type declarations; shared read-only borrows only. Criteria 1-5 PROVEN (lifetime-parameterized nominal parse/kind-check, fixed borrowed record/tuple construct-copy-project-return, projection-aware loan facts, loan fact model, function-value lifetime relations); criteria 6 and 7 PARTIAL, and 8,9,10,11 MISSING. Row 6 now supports checked mutable shells updating owned scalar fields while transporting the existing hidden root set; borrowed-field replacement and old/new root sequencing remain. Remaining before implemented: field-replacement loan sequencing (row 6), aggregate retain/drop balance matrix incl. early-return/?/branch/loop/UAF (row 8), List(B('a)) lifecycle (row 9), a runnable zero-copy parser + borrowed iterator with zero-materialization counters (row 10), and the complete shipped-contract spec/book/reflection docs (row 11). Per rfcs/0110-0112-acceptance-ledger.md."
predecessors:
  - "[0024](0024-unified-facts-lattice.md) (shared facts and confinement lattice)"
  - "[0083](0083-opt-mode-lifetimes.md) (`let('a)` inputs, `View(T, 'a)` results, and owner loans)"
  - "[0110](0110-opt-ownership-access-abi.md) (exact owner/access relations across callables)"
related:
  - "[0027](0027-packed-layouts-sroa.md) (typed physical layout and escape-driven scalar replacement)"
  - "[0051](0051-memory-safety-invariants.md) (object-base classification and safe RC operations)"
  - "[0111](0111-cross-boundary-specialized-layouts.md) (layout descriptors consumed by borrowed aggregate lowering)"
---

# RFC-0112: Borrowed aggregate types and projection-aware loans

> Provisional new syntax is limited to lifetime parameters on nominal type
> declarations, such as `type Parser('a):`. Code blocks are intentionally not
> tagged `witchy` until the syntax is implemented.

## Summary

Allow a `mode opt` nominal type to contain read-only borrowed views whose owner
lifetimes are explicit type parameters. Make loan facts projection-aware so a
view of a field, list window, parser span, or nested borrowed aggregate can be
stored, returned, forwarded, and finally consumed without materializing an owned
copy or losing the root that keeps its storage alive.

RFC-0083 ships function-level lifetime relations:

```text
fn inspect(let bytes: let('a) Bytes) -> View(Bytes, 'a)
```

It deliberately rejects persistence inside owned aggregates and requires a
projection of an already-bound view to be materialized. That is a sound first
boundary, but it prevents zero-copy parsers, iterators, cursors, token streams,
and multi-field protocol readers from being ordinary typed values.

This RFC adds one surface form:

```text
type Parser('a):
    input: View(Bytes, 'a)
    offset: Int
```

Lifetime parameters are compile-time relations, not runtime values. Borrowed
aggregates remain read-only with respect to their owners, carry no pointer
identity, cannot outlive any owner they name, and cannot cross a suspension or
ownership boundary that erases the relation. Mutable references remain out of
scope.

## Motivation

### Functions can return a view, but user types cannot retain it

The current lifetime system handles a direct borrowed result well. Real zero-copy
APIs quickly need more structure:

- a parser holds an input view plus a cursor;
- a token holds a lexeme view plus source coordinates;
- an iterator holds a collection view plus an index;
- a decoder holds views into two input buffers;
- a zipper returns several related projected views; and
- a trait adapter carries a view plus a witness-independent scalar state.

Materializing every projection restores ownership but gives up the memory and
cache behavior the lifetime contract exists to provide.

### Projection identity is a safety fact

A view is not necessarily an owning object base. Treating an interior pointer as
an RC object caused the SEC-037 class closed by RFC-0051. The type system must
retain both:

- the owner root whose lifetime keeps storage valid; and
- the projection path/range that determines how the view is read.

This information cannot be reconstructed from an unqualified inner type after
lowering.

### Borrowed aggregates are not shared mutable references

The aggregate stores read-only views. Any number may coexist, and the owner may
be read, but the owner cannot be moved, mutated, written back through `var`, or
consumed through `own` until every dependent aggregate's final use. Mutation of
the aggregate's own scalar cursor is permitted when the aggregate binding is a
`var`; it does not mutate viewed owner storage.

This preserves Witchy's acyclic ownership model and copy-correct value oracle.

## Syntax

### Lifetime parameters on nominal types

A nominal type may declare lifetime parameters alongside its existing type
parameters:

```text
type Parser('a):
    input: View(Bytes, 'a)
    offset: Int

type PairView(a, 'left, 'right):
    first: View(a, 'left)
    second: View(a, 'right)
```

Lifetime names begin with `'`, are implicitly universally quantified, and occupy
a separate kind from ordinary type parameters. `a` and `'a` are distinct. A
lifetime argument may appear only where a borrowed qualifier expects one.

The first implementation permits lifetime parameters on named-field records and
single-variant positional types. Multi-variant sums join after every variant's
root/drop layout is representable and tested.

### Construction and inference

Construction infers lifetime arguments from borrowed fields:

```text
fn parser(let input: let('a) Bytes) -> Parser('a):
    Parser(input.view(0, input.length()), 0)
```

Every lifetime in a constructed aggregate must resolve to a live owner loan.
There is no inferred `'static`, and a view of a temporary cannot be persisted.
An explicit type ascription may disambiguate multiple owner positions but cannot
extend a lifetime.

### Function and trait types

Borrowed aggregate arguments/results carry the same output-to-input relations as
`View`:

```text
fn next(let parser: let('a) Parser('a)) -> Parser('a)
fn lexeme(let parser: let('a) Parser('a)) -> View(Bytes, 'a)
```

RFC-0110's callable identity includes every lifetime parameter and owner position.
An ascription or witness adapter that erases them is rejected.

Lifetime elision remains limited to an unambiguous single owner. Public APIs with
multiple possible owners spell each relation explicitly.

## Semantics

### Owner sets

Every borrowed value has an `OwnerSet`: one or more stable owner roots plus a
projection descriptor. A borrowed aggregate's owner set is the union of the
owner sets of its borrowed fields.

An output lifetime must be supplied by:

- an input `let('a)` owner;
- an input borrowed aggregate carrying `'a`; or
- a lexically named region relation introduced by a future RFC.

Fresh allocation inside the function cannot manufacture an output lifetime.

### Loan duration

Constructing or binding a borrowed aggregate opens loans on every root in its
owner set. Each loan ends at the aggregate's last use, with non-lexical precision.
Copying a borrowed aggregate creates another shared read loan; every copy must
reach its final use before the owner becomes mutable or movable again.

Destructuring transfers the corresponding field loans to the bound fields.
Discarding an unused borrowed field closes that field's contribution immediately
when the facts prove no other field depends on the same root.

### Projection-aware conflicts

The checker records projection paths and ranges:

- nominal fields have static paths;
- tuple positions have static paths;
- list/string/bytes views have a root plus a checked range expression; and
- a projection of a view composes its path/range with the existing descriptor.

Shared read projections may overlap. Any live projection blocks mutation or move
of its owner root. The first implementation does not use disjoint read ranges to
permit mutation elsewhere in the same owner; that refinement requires an
exclusive-borrow RFC and is deliberately absent.

### Analysis phases and performance contract

The checker is specified as a point-based loan analysis. A loan fact names its
owner root, borrower projection, and the program point where it opens, remains
live, transfers, or closes; lowering consumes those same facts. This keeps the
borrow relation independent of an AST spelling or a particular lowering path.

The initial implementation is an **alpha** analysis. A cheap pass collects
owner-root and projection facts, identifies candidate invalidations, and limits
the precise reachability calculation to the affected control-flow component. A
conservative prepass may request that calculation but may not itself reject a
program. A rejection requires an active loan at the invalidating program point.
This allows conditional returns, branches, and loops to become more precise
without weakening the owner-root contract or requiring a whole-program
re-analysis on every borrow.

The performance objective is Rust-class predictable overhead for zero-copy
`mode opt` programs, measured rather than assumed. Each precision milestone
must record compile time, loan count, constraint-edge count, and generated
allocation/retain/drop counters on a pinned corpus. The first benchmark report
sets the project thresholds; it must compare equivalent scalar and zero-copy
workloads, publish percentile results and outliers, and separate analysis cost
from Wasm code generation. No RFC claim of Rust parity is valid until that
repeatable baseline exists.

Future work may add more precision in this order:

1. control-flow-sensitive conditional borrows and loop reborrows;
2. callable access summaries for disjoint field projections; and
3. richer internal-reference and borrowed-container forms.

Those phases add facts and validation evidence, not new user syntax. They do
not imply general mutable references, address identity, or loans across
`await`/`yield`.

### Aggregate shell mutation

A borrowed aggregate may be held in a mutable binding and update owned scalar
fields:

```text
var p = parser(input)
p.offset += 1
```

The update writes back the aggregate shell while retaining its declared lifetime
relations. Reassigning a borrowed field is allowed only when the replacement
satisfies that declared relation and all old/new loan events are ordered by the
checked statement facts. The replacement may name a different runtime owner:
RFC-0110's write-back envelope transports the updated root set, while the
callable type preserves the lifetime position rather than pretending to encode
owner identity.

### Escape rules

A borrowed aggregate may be:

- passed to a `let` parameter preserving its lifetimes;
- returned when every output lifetime is bound by an input relation;
- destructured or projected;
- copied into another borrowed aggregate whose lifetime parameters preserve the
  same owner set; and
- captured by a closure proven non-escaping and invoked within every owner
  lifetime.

It may not be:

- stored in an ordinary owned aggregate that erases its lifetime parameters;
- placed in `Dynamic` or an owned existential;
- sent through a channel or isolated worker;
- captured by an escaping closure or task;
- held live across `await` or `yield` in the initial model;
- passed to `own` or a relation-erasing function value; or
- reflected/serialized as an address-bearing representation.

Materialization is explicit and typed. `.owned()` remains available for a view
field and for a borrowed aggregate only when its API declares an owned companion
result, such as `Parser('a) -> ParsedInput`. Otherwise the program destructures
the aggregate and owns the required fields explicitly. Witchy does not invent a
lifetime-erased nominal type or infer a user conversion.

## Representation and reclamation

A borrowed aggregate is a typed shell containing logical view values plus hidden
owner roots. The layout descriptor distinguishes:

- an owning object reference;
- a non-owning projection/view;
- the root retained on behalf of that view; and
- ordinary scalar or owned fields.

Compiled lowering retains each distinct linear-memory owner root exactly once per
live aggregate owner relation and releases it after the checked last use on
fallthrough, explicit return, and `?` propagation. Typed GC roots remain typed
references rather than integer slots. Host-backed roots require a separate
lease-bearing capability API and do not become valid merely by using `'a`.

Dup/drop emission follows RFC-0051's static object-base classifier. It may never
emit an owning `$rc_dup` or `$rc_drop` on a view address or projection pointer.
Copying a borrowed aggregate retains its roots, not its interior addresses as
owners.

The interpreter may materialize fields but consumes the same loan facts and
rejects the same source programs. Forced-copy compiled mode is the value oracle;
it cannot bypass owner conflicts.

## Generics, traits, and containers

### Generics

Lifetime and type parameters are separately kind-checked. Monomorphization keys
include concrete type arguments and symbolic lifetime relations, but lifetimes
do not create runtime-specialized copies when physical layout is otherwise
identical.

### Traits

A trait method may accept or return a borrowed aggregate when it is
existential-safe under the same rule as a direct `View`: every returned lifetime
is tied to an explicit receiver/argument owner position. A bare hidden-receiver
borrow remains excluded from `dyn Trait` until witness metadata can authenticate
that owner relation.

### Containers

The first implementation does not permit `List(Parser('a))`, `Dict(k,
Parser('a))`, or a multi-variant borrowed sum. Their element drop/root layout must
become a first-class descriptor before RC and packed transformations can move
them safely. Fixed nominal borrowed records and tuples are sufficient for parser,
iterator, cursor, and zipper APIs and form the initial acceptance scope.

Container support is required before this RFC may be marked implemented: after
the fixed-shell phase is green, `List(B('a))` must retain each distinct owner
correctly, release on overwrite/drop, and reject every relation-erasing boundary.
`Dict` and borrowed existentials remain follow-on RFCs unless a measured workload
requires them.

## Analysis architecture

RFC-0083's statement-identity loan events become projection-aware CFG/SSA facts:

```text
BorrowFact {
    borrower,
    owner_root,
    projection,
    lifetime,
    open_point,
    close_points,
}
```

Joins union live owner sets conservatively. A branch may close a loan at distinct
last-use points on each path. Loops compute a fixpoint; an unresolved back-edge
keeps the loan live and reports the exact edge if that blocks mutation. No
AST-local second loan engine is allowed.

## Diagnostics

Diagnostics name:

- the borrowed aggregate and field;
- the original owner root and borrowing call/construction;
- the projection carried through wrappers;
- the escape, mutation, move, suspension, or type erasure that conflicts;
- the path-sensitive final use when available; and
- the repair: shorten the aggregate's use, preserve the lifetime parameter,
  destructure it, or materialize with `.owned()`.

They never report a view address or hidden root local.

## Verification

### Static matrix

Checker tests cover:

- one and multiple owners;
- construction, forwarding, return, copy, destructure, and projection;
- nested borrowed aggregates;
- scalar shell mutation and borrowed-field replacement;
- direct, trait, closure, and function-value calls;
- normal callers of opt APIs;
- branch/loop last use;
- temporary, owned-aggregate, closure, task, channel, async, generator,
  existential, reflection, and serialization escapes; and
- relation-erasing ascriptions.

Each rejection checks the owner and conflicting statement in the diagnostic.

### Runtime matrix

Interpreter, optimized Wasm, forced-copy Wasm, RC poison/no-reuse mode, and
checked-heap mode run parser/iterator fixtures with independent expected output.
Counters prove root retain/drop balance, zero view materialization on accepted opt
paths, and exactly one materialization in paired `.owned()` cases.

An adversarial UAF corpus covers nested projections, copied borrowed aggregates,
owner overwrite after last use, early return, `?`, branch joins, loop back-edges,
and container element overwrite once container support lands.

## Acceptance criteria

1. Lifetime parameters on nominal types parse, format, kind-check, reflect as
   type relations, and are restricted to `mode opt` declarations.
2. Fixed-layout borrowed records/tuples construct, copy, destructure, project,
   forward, and return while preserving exact owner sets across module boundaries.
3. Projection of an already-bound view may be persisted without materialization;
   lowering retains the original owning root, never the projection as an RC base.
4. Non-lexical projection-aware facts allow owner mutation immediately after the
   final aggregate/view use and reject it on every live path before that point.
5. Direct, trait, closure, and function-value types preserve lifetime and owner
   positions through RFC-0110's access signature; erasing adapters reject.
6. Scalar shell mutation preserves lifetime relations; borrowed-field
   replacement sequences old/new loans and transports the updated runtime root
   set through `var` write-back.
7. Every documented escape/suspension boundary rejects with an actionable
   diagnostic; a typed owned-companion conversion or field `.owned()` supplies
   the explicit materialization escape hatch.
8. Root retain/drop, early-return, `?`, branch, loop, poison/no-reuse, and UAF
   tests prove no premature free and no leaked root.
9. `List(B('a))` supports construction, traversal, copy, overwrite, and drop with
   correct owner rooting and rejects relation-erasing boundaries.
10. A runnable zero-copy parser and borrowed iterator use borrowed aggregate
    types on both backends; counters prove zero materialization in the compiled
    opt path.
11. `spec/language.md`, `spec/performance.md`, reflection, docs generation, and
    the book state the same lifetime/escape rules.

The RFC remains `proposed` until all eleven criteria are proven in a checked
acceptance ledger. Fixed-shell support without borrowed-container completion is
an implementation phase, not completion.

## Staging and dependency graph

1. **Syntax and kinds.** Lifetime parameters on nominal declarations; signature
   validation only, no runtime representation change.
2. **Projection facts.** Move RFC-0083 loans onto projection-aware CFG/SSA facts
   and delete the materialize-on-persist limitation for fixed shells.
3. **Callable integration.** Consume RFC-0110's frozen access signature across
   direct, trait, closure, and function-value calls.
4. **Runtime roots.** Typed root retention/drop and the UAF corpus.
5. **Borrowed containers.** Descriptor-driven `List(B('a))` ownership roots.
6. **Examples, docs, and acceptance ledger.** Parser and iterator prove the
   intended application rather than a syntax-only feature.

RFC-0110 stages 1–2 are prerequisites. RFC-0111 may proceed in parallel after
that interface freezes; RFC-0112 consumes its descriptor machinery only for the
borrowed-container stage.

## Alternatives

- **Return tuples of views instead of named types.** Useful for small cases but
  cannot express reusable parser/iterator APIs or preserve relations through
  nominal abstraction.
- **Materialize every projection.** Safe and already available through
  `.owned()`, but defeats zero-copy parsing and makes wrapper functions allocate.
- **Infer aggregate lifetimes without syntax.** Rejected for public APIs. Owner
  relations are compatibility and safety contracts and must be reviewable.
- **Add Rust-style `&T` fields.** Rejected. `View(T, 'a)` already names the
  read-only relation without exposing pointer identity or a second reference
  syntax.
- **Add mutable borrowed fields now.** Rejected. They introduce observable
  mutation through an alias and require a separate decision about exclusivity,
  reborrowing, and Witchy's value-semantics invariant.

## Drawbacks

- Lifetime parameters become part of public type compatibility and error
  messages, adding conceptual weight to the opt surface.
- Projection-aware non-lexical facts require shared CFG/SSA work; a conservative
  join can reject a safe program until the analysis learns the missing relation.
- Borrowed aggregates retain owner roots. They avoid payload materialization but
  can extend an owner's lifetime and increase peak memory when held too long.
- Container support adds nontrivial root/drop metadata and is intentionally an
  RFC-completion requirement rather than being hidden as follow-up work.
- The initial suspension restriction makes some async parser/iterator designs
  materialize until a later structured-suspension proposal proves them safe.

## Prior art

- [Counting Immutable
  Beans](../external-refs/counting-immutable-beans-2019/notes.md) combines RC
  with inferred borrowed references to suppress retain/release traffic.
- [Implementation Strategies for Mutable Value
  Semantics](../external-refs/mutable-value-semantics-2022/notes.md) supplies the
  read/exclusive/consume access model that keeps these borrows read-only.
- Rust's lifetime-parameterized structs and non-lexical lifetimes demonstrate
  that owner relations can survive nominal aggregation; Witchy keeps `View` and
  value semantics instead of adopting general reference fields.

## Non-goals

- No mutable or shared-mutable references.
- No pointer identity, address inspection, weak references, or finalizers.
- No loan across `await`/`yield` in the initial implementation.
- No automatic host-resource lifetime from ordinary `'a`; host buffers require
  lease-bearing capability APIs.
- No `Dict` of borrowed values or borrowed `dyn Trait` in this RFC.
- No claim that lifetime syntax alone improves performance; zero materialization
  must be proven by counters and end-to-end examples.
