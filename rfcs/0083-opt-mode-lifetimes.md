---
rfc: 0083
title: Opt-mode lifetimes and returnable borrowed views
status: accepted
created: 2026-07-12
superseded-by:
tracking: >
  Phase 1 (static lifetime + loan foundation) implemented 2026-07-14: `let('a) T`
  / `View(T, 'a)` surface (a `Qualified(TypeQual::Borrow)` type, no runtime
  representation — erased to the owned type before both backends, parity by
  construction); signature relations validated and carried across calls / trait
  dispatch / function values / module boundaries; per-caller owner loans with
  non-lexical last-use and `func.owned(view)` materialization; rejection of owner
  move / mutate / reassign / `var`|`own` write-back / closure|task|channel escape
  while a view is live (`crates/witchy-types/src/loans.rs`). NOT YET DONE (next
  phase): compiled-backend owner rooting / host leases (acceptance 5), ownership-
  analysis treating a live loan as non-unique for in-place update/extraction
  (acceptance 6), and the `view.owned()` method-call spelling (blocked on generic
  free-function UFCS; the free-call `func.owned(view)` is the phase-1 spelling).
related:
  - "0029 (optimization contract - missed facts copy or reject in opt mode)"
  - "0087 (uniform var write-back - active returned views block owner write-back)"
  - "0088 (ownership-aware extraction - active loans constrain in-place extraction)"
---

# RFC-0083: Opt-mode lifetimes and returnable borrowed views

## Summary

Complete the tier-4 work anticipated by RFC-0026, RFC-0028, RFC-0029, and
`performance-modes.md`: `mode opt` files may name lifetime relationships and
return read-only borrowed views without copying. Lifetimes describe
representation validity, not new observable aliasing. Normal witchy retains
owned value semantics and no lifetime syntax requirement. A returned view still
carries its owner obligation across the mode boundary: normal callers do not
spell lifetimes, but the checker enforces the callee's declared relation.

The first version deliberately excludes mutable references. Read-only borrows
can be materialized to owned values as a differential oracle; observable mutable
aliasing could not.

## Motivation

Witchy's current conventions solve the common case: `let` borrows within one
call, `own` transfers values, `var` writes back, `unique` guarantees isolation,
and confined views optimize local slices. The missing cases are values whose
borrowed representation must cross a function boundary:

- returning a substring or byte slice;
- parsers returning fields borrowed from an input buffer;
- iterators retaining a collection borrow;
- zero-copy protocol and file-format readers;
- borrowed `dyn Trait` adapters;
- FFI and component adapters over host-owned buffers.

RFC-0028 explicitly defers these to lifetime inference, and performance modes
names them as the one genuine analytical power increase reserved for `mode opt`.

## Design

### Surface

```witchy
mode opt

fn first_line(text: let('a) String) -> View(String, 'a):
    text.view(0, text.index_of("\n") ?? text.length())

fn field(record: let('a) Record) -> View(Bytes, 'a):
    record.bytes.view(record.start, record.end)
```

`let('a) T` is a read-only borrow of `T` carrying lifetime `'a`. Lifetime names
are implicitly quantified by a function signature, as lowercase type variables
are today; they are not generic runtime arguments or values. Elision applies to
the common single-input case, so explicit names are required only when several
input lifetimes compete or a public contract would be ambiguous.

The exact surface may shorten `View(String, 'a)` to `StringView<'a>` in std; the
semantic distinction is owner type, range, and lifetime.

### Borrow kinds

The initial model has one borrow kind: shared read-only. Any number of read-only
views may coexist. While a view is live, mutation or move of the storage it
references is rejected. The owner may still be read.

There is no `&mut`, shared mutable reference, reference field in an ordinary
record, or arbitrary pointer arithmetic. Mutation remains `var` write-back or
unique ownership.

### Lifetime relations

The checker tracks:

- lexical local scopes;
- function input and output lifetime parameters;
- `region:` ancestry;
- closure capture and task/channel escape;
- owner move, mutation, and last use;
- host-call validity for externally backed views.

An output lifetime must be bounded by an input or explicitly named region. A
borrow cannot be returned as `'static` merely because its owner is currently
frozen. Sending a view through a channel or storing it in an owned `Dynamic`
value requires materialization unless the receiver's scope is statically within
the same lifetime.

The typed function convention records each returned view's dependency on an
input lifetime. That relation survives direct calls, trait dispatch, function
values, and module boundaries. At a call site, the returned value creates a loan
of the corresponding owner. The loan ends at the view's last use, or when the
view is consumed by `.owned()` and the owned result no longer depends on the
source storage.

### Public APIs and mode boundaries

A public function may expose a borrowed result only from a `mode opt` module.
Normal callers can consume it in a statically bounded scope or call `.owned()`
to materialize. Type inference inserts no hidden long-lived borrow, but it does
instantiate and enforce the borrow relation already present in the function
type. Normal source does not need lifetime syntax for this enforcement.

At a normal-to-opt boundary, ordinary owned arguments satisfy borrowed inputs.
At an opt-to-normal boundary, a borrowed result must be consumed locally or
materialized before entering an API that promises ownership.

While that returned view is live, every caller mode rejects moving or mutating
its owner, including passing the owner to `own` or `var`, assigning through an
owner place, or carrying an unresolved loan across task/channel escape. This is
the same borrow rule as inside `mode opt`; a mode boundary cannot erase it.

### Cross-mode enforcement and representation

The return-to-input lifetime relation is a typed call fact, not an AST-local
heuristic and not merely an escape-summary bit. The normal-mode type checker
consumes it when checking the caller body. The ownership/uniqueness analysis
consumes the same active-loan fact: an owner with a live shared loan is not
available for an in-place mutation or extraction path, even when its reference
count happens to be one.

The compiled representation must also keep the borrowed storage alive. A view
either retains a runtime root/lease for its owner or is proven not to outlive an
already-live owner local; the root is released after the view's last use. Host
views carry the corresponding host lease. `Summaries::arg_leaks` may transport
part of this information, but it is not the semantic source of truth and cannot
replace the function type's lifetime relation.

The interpreter may materialize views for simplicity, but it must enforce the
same source-level owner loan. Forced-copy differential mode is a value oracle,
not permission to accept a mutation that the borrowed representation rejects.
Users who need to mutate the owner first materialize with `.owned()` and end the
view's use.

### Semantic parity and fallback

Every read-only borrowed operation has an owned reference implementation used by
the interpreter and the forced-copy differential mode. Replacing a view with an
owned copy must preserve all observable results. Therefore `mode opt` changes
representation and can turn a missed proof into a compile error, but does not
change value semantics, consistent with RFC-0029.

Borrow identity, pointer identity, and mutation-through-view are unobservable.
Equality compares viewed contents. Reflection reports the logical viewed value,
not its address or storage owner.

### Capabilities and external resources

A capability is not ordinary borrowed data. Borrowing a capability value cannot
extend its grant or cross a VM boundary. A host-backed byte view, such as a mapped
file or receive buffer, carries both a data lifetime and an unforgeable host lease;
the host lease is released only after every view expires. Such APIs require a
separate capability-specific design and are not implied by this RFC's linear-
memory views.

### Analysis and diagnostics

Lifetime checking consumes the shared compiler Facts/CFG substrate anticipated by
the performance RFCs. It must not become a second AST-local fact engine.
Diagnostics name the owner, borrow creation, required lifetime, conflicting move
or mutation, and the `.owned()` escape hatch.

The diagnostic at a normal-mode call site also names the opt-mode API whose
return type created the loan. Optimization diagnostics distinguish a live loan
from ordinary refcount sharing; silently treating a live loan as unique would be
a soundness bug, not a missed optimization.

## Acceptance criteria

1. Typed direct, trait, and indirect calls preserve an output-to-input lifetime
   relation; erasing conventions at a function-value boundary is rejected.
2. Normal and opt callers reject owner mutation, move, and `var` write-back
   while a returned view remains live, with matching diagnostics.
3. Last-use analysis and `.owned()` end the loan without extending it to the
   owner's whole lexical scope.
4. Nested owners, views returned through one wrapper function, and multiple
   shared views retain the correct root and reject the same conflicts.
5. WIR keeps owner storage or its host lease alive until the last view use;
   refcount, trap, and early-return tests detect premature release and leaks.
6. Ownership analysis treats an active view loan as unavailable for in-place
   update or extraction. Forced-copy and optimized executions remain value
   equivalent.
7. Async, closure, task, channel, record, and `Dynamic` escape cases either
   prove the lifetime relation or require materialization.
8. Interpreter and compiled-backend tests cover every boundary above, including
   a normal caller of an opt API.

## Alternatives

- **Copy every returned slice.** Keeps the language simple but blocks zero-copy
  parsing and buffer-oriented application domains.
- **Expose lifetimes in normal mode.** More uniform, but taxes code that benefits
  from Witchy's value-semantic conventions and contradicts the performance-tier
  contract.
- **Add mutable Rust-style references immediately.** More powerful, but creates
  observable aliasing and would require reconsidering RFC-0029 rather than merely
  completing it.
- **Infer all lifetimes.** Attractive locally, but public APIs need explicit,
  reviewable relationships and inference failures need a vocabulary for fixes.

## Drawbacks

- Introduces advanced syntax and borrow-checker diagnostics.
- Mode boundaries and materialization rules add API-design choices.
- Precise interprocedural lifetime facts likely require CFG/SSA work.
- Borrowed host resources remain a separate hard problem.
- Normal code can receive a borrow diagnostic after calling an opt API even
  though normal source never spells a lifetime. The diagnostic must make the
  hidden function-type relation and `.owned()` remedy explicit.

## Prior art

Rust lifetimes, Swift borrowing, Hylo access lifetimes, Vale regions, C++ views,
and Cyclone region typing inform this design.

## Implementation status

> 2026-07-14: Phase 1 (static foundation) landed. What shipped, mapped to the
> acceptance criteria above:
>
> - **Surface & representation.** `let('a) T` and `View(T, 'a)` parse (a `'a`
>   lifetime token) and round-trip through `witchy fmt` (canonically as
>   `View(T, 'a)`). A view is `Type::Qualified(TypeQual::Borrow(lifetime), inner)`
>   — deliberately a *qualifier*, not a new `Type` variant, so it inherits every
>   existing `Qualified` code path and has **no runtime representation**: `to_ty`
>   erases it to the owned inner type before either backend, so a view lowers
>   exactly as its owner and value semantics are unchanged (RFC-0029 consistency;
>   the differential/`parity` runs and a runnable `book/` example confirm it).
> - **Criteria 1–4, 7 (checker):** met by `crates/witchy-types/src/loans.rs` —
>   signature validation (views are `mode opt`-only; every output lifetime must be
>   bound by an input of the same name), an output→input relation read off the
>   signature (so it survives direct/trait/indirect calls, function values, and
>   module boundaries), per-caller owner loans with non-lexical last-use, and
>   rejection of owner move / reassign / mutate / `var`|`own` write-back / closure
>   & channel & task escape while a view is live. Diagnostics name the owner, the
>   borrowing call, and the materialization remedy.
> - **Criterion 8 (checker half):** covered by `loans_tests.rs` plus a both-backends
>   `book/` example. The *compiled-backend* half is part of the next phase.
>
> **Deferred to the next RFC-0083 phase (NOT yet implemented):**
>
> - **Criterion 5** — compiled WIR keeping owner storage / host leases alive to the
>   last view use (refcount/trap/early-return leak tests). Phase 1 is sound because
>   views are erased to owned values (the owner is a live local for its lexical
>   extent); true zero-copy rooting is the runtime-representation work.
> - **Criterion 6** — ownership analysis (`witchy-lower`) treating a live loan as
>   non-unique for in-place update/extraction (RFC-0088 interaction). Needs the
>   loan facts to flow from typeck into the lowering analysis.
> - The **`view.owned()` method spelling.** Materialization is `func.owned(view)`
>   in phase 1; the method form is blocked on generic-free-function UFCS dispatch
>   (an `x.method()` on a concrete receiver does not yet resolve to a generic free
>   function — a pre-existing limitation, not specific to views), so it is not a
>   per-method dispatch hack this RFC should add.
