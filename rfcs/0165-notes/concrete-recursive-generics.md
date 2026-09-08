# Concrete recursive generic layout evidence

The source language infers lowercase type parameters from declaration fields.
`TypeDef.params` contains only explicit header parameters. Canonical aggregate
shape validation and substitution now use `effective_type_def_params`, matching
the source type checker rather than refusing valid inferred instantiations.

`concrete_aggregate_identity` normalizes checked declaration identity and
concrete arguments without expanding bodies. Qualified names require exact
lookup; ambiguous bare names refuse. A bare definition and its sole equal
qualified alias use the qualified key. Identity work is bounded by depth and
node count. Body validation remains in the separate bounded shape checker.

Catalog traversal, recursive cycle discovery, nominal hashing, and lowering
lookup use that identity. Concrete arguments remain part of the key, so
`Stack(Int)` and `Stack(String)` cannot share a nominal identity. Physical
layout validation and final source/layout correspondence remain mandatory.

Focused evidence:

- `canonical_aggregate_identity`: three tests passed for inferred parameters,
  qualified/bare identity, ambiguous declaration rejection, and bounded work.
- `concrete_recursive_generic_nominals_are_distinct_stable_and_closed`: passed
  for distinct bound nominal roots, visitation-order-independent bundle bytes,
  and unresolved argument rejection.
- `generic_shapes_refuse_unresolved_wrong_arity_and_expanding_or_inline_cycles`:
  passed; generic identity does not replace the body validator.
- `rfc0165_recursive_generic`: the same checked module instantiates a recursive
  stack with both Int and String. Source interpreter, SafeWIR oracle, and
  admitted Wasm return the independent expected `5`; both heap audits report
  zero leaks. Passed in 0.30 seconds.

Reproduce with the existing warmed target:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test --test rfc0165_recursive_generic -- --nocapture
```

The real `generic_stack` example advances past layout construction but still
fails initial validation with `TailSiteFunctionMismatch`: expected FunctionId(11),
actual FunctionId(7). It is not counted as admitted or agreeing. That source
tail-obligation binding needs a separate fix; no obligation was dropped here.

No new full gate or fresh classified master corpus has run. The nullable
collection regression and unfinished hex fixture remain separate blockers.
