# BUG-458: Type argument arity is checked

Severity: MED
Status: FIXED (this commit)
Verified: 2026-07-07 on master 728a94d before fix; `Int(String)` and bare
`List` reproduced as accepted, while malformed user ADT arity leaked to later
type mismatch diagnostics.
Component: type checker, generic type validation, builtin types, ADT APIs,
diagnostics

## Resolution

`check_type_names` now validates arity for every `Type::Named` it accepts:

- zero-arity builtins reject ordinary type arguments;
- `List`, `Option`, `Set`, and `Iter` require one argument;
- `Result` and `Dict` require two arguments;
- user ADTs use explicit `type T(a, b)` parameters when present, otherwise the
  same first-appearance field-parameter inference the checker already uses;
- synthetic tuple heads (`TupleN`) require `N` arguments;
- `Dir`/`File`/`Net` remain capability-right marker forms, not ordinary type
  applications.

The validator now also walks body ascriptions, lambda annotations, `as` targets,
region return annotations, trait method signatures/defaults, impl target/trait
arguments, impl bounds, consts, and type aliases. A pre-trait-lowering pass keeps
source-quality diagnostics; the existing lowered-function check remains in place.

Regression: `type_argument_arity_is_checked` covers malformed scalar, builtin
generic, inferred ADT, explicit ADT, and local-ascription cases, plus valid
generic and capability-marker controls.
