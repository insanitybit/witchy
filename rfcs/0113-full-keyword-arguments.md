---
rfc: 0113
title: "Full keyword arguments: support UFCS method calls"
status: implemented
created: 2026-08-02
predecessors:
  - "[0056](0056-keyword-arguments.md) (keyword arguments and defaults shipped for direct calls)"
  - "[0050](0050-method-call-generalization.md) (UFCS method resolution and function values)"
tracking:
---

# RFC-0113: Full keyword arguments — support UFCS method calls

## Summary

This RFC extends the existing keyword-argument and default-argument support from
free/module direct calls to UFCS method calls as well. A method call like
`s.substring(start: 1, end: 3)` resolves its labels against the chosen method
declaration and is rewritten before typing and codegen into the same positional
shape that both backends already expect.

Method calls are still the one remaining call shape where labels cannot exist in the
function value itself:

- Direct calls (free/module/function names and UFCS methods) may use labels.
- Calls through function values (`Apply`, including trait-adapted values) remain
  positional-only.

## Motivation

`0056-keyword-arguments` removed the constructor-only asymmetry for labels and
defaults, but left method calls out of scope as a v1 limitation. That limitation
is now unnecessary because UFCS method resolution already produces a concrete method
declaration before keyword handling in the pipeline, and the implementation is
already doing the same positional rewrite that makes parity cheap.

For users, the practical gap is real:

- `connect(host, tls: false)` is readable.
- `payload.substring(start: 1)` should be equally readable, with the same source
  behavior.

For implementers, parity remains intact:

- The keyword pass never changes runtime shape.
- Both interpreters and compiled backend still observe positional calls.

## Design

### Supported direct-call forms

The following are rewritten from labeled syntax to positional calls after linking:

1. Free and module-qualified direct calls
2. UFCS method calls with a resolved receiver (`receiver.method(...)`)
3. Record constructors, which already participate in the same rewriting discipline

### Label syntax and arity rules

- A call may contain an arbitrary mix of positional arguments followed by labeled
  arguments (`positional ... , label: expr`).
- A positional argument after a labeled one is illegal.
- Label names must match the resolved declaration parameter names.
- Every parameter must be bound exactly once.
- Omitted trailing parameters with closed-constant defaults are spliced.

These rules apply equally in method and non-method direct calls, and the same
link-level checks raise unknown/duplicate/missing-parameter errors.

### Method-call pipeline interaction

UFCS method labels are not resolved in a vacuum:

1. The parser accepts labeled syntax uniformly in call argument lists.
2. Existing method resolution identifies the concrete method implementation.
3. The keyword-argument rewrite pass rewrites `LabeledMethodCall` by name lookup and
   `MethodCall` with positional arguments in the declaration order.

If no resolved method exists (or method lookup remains ambiguous), existing method
resolution diagnostics continue to run; the label path is not the authority on
ambiguity.

### Defaults

Closed-constant defaults apply to UFCS methods as they do to free/module calls.
Defaults stay an attribute of the declaration; they do not become part of the
function type or any callable value.

### Source-order evaluation

As in RFC-0056, evaluation order remains source order. Any label-induced reorder
binds values to temporary identifiers first, left-to-right as written, then passes
them into the positional call in declaration order.

## Alternatives

- **Keep method-call labels excluded.** Lower implementation cost, but inconsistent
  call ergonomics and incomplete keyword-argument support.
- **Admit labels only to function types.** This collides with the function-value
  model and creates a larger typing/language interaction.

## Drawbacks

- Parameter renames continue to be source-breaking for labeled callers.
- Labeled calls can appear in two syntactic forms (`f(a,b)` and `f(a:..., b:...)`);
  style becomes a convention issue.

## Prior art

- **Python**: keyword arguments with defaulted optional parameters.
- **RFC-0056 / 0050** (internal): Witchy already solved parse/lower parity for
  this shape in non-method paths and already resolved UFCS method calls in the
  front-end pipeline.
- **OCaml**: labeled arguments at the call boundary are informative for reader
  intent; Witchy keeps labels out of function types.

---

## Implementation note (2026-08-02) — status: implemented

Shipped in `crates/witchy-syntax/src/keyword_args.rs` and covered by
`src/example_tests/keyword_args.rs`:

- Parser now keeps UFCS labels as `Expr::LabeledMethodCall` in `postfix()`.
- The existing method-resolution pass still runs first; then the keyword-arg pass
  resolves the concrete method declaration and rewrites labels to positional
  arguments on `Expr::MethodCall`.
- Unknown labels, duplicate binds, and missing parameters are reported as link
  errors, and direct default omission remains source-constant and declaration-side.
