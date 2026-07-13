---
rfc: 0083
title: Opt-mode lifetimes and returnable borrowed views
status: proposed
created: 2026-07-12
superseded-by:
tracking:
---

# RFC-0083: Opt-mode lifetimes and returnable borrowed views

## Summary

Complete the tier-4 work anticipated by RFC-0026, RFC-0028, RFC-0029, and
`performance-modes.md`: `mode opt` files may name lifetime relationships and
return read-only borrowed views without copying. Lifetimes describe
representation validity, not new observable aliasing. Normal witchy retains
owned value semantics and no lifetime syntax requirement.

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

### Public APIs and mode boundaries

A public function may expose a borrowed result only from a `mode opt` module.
Normal callers can consume it in a statically bounded scope or call `.owned()`
to materialize. Type inference inserts no hidden long-lived borrow.

At a normal-to-opt boundary, ordinary owned arguments satisfy borrowed inputs.
At an opt-to-normal boundary, a borrowed result must be consumed locally or
materialized before entering an API that promises ownership.

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

## Prior art

Rust lifetimes, Swift borrowing, Hylo access lifetimes, Vale regions, C++ views,
and Cyclone region typing inform this design.
