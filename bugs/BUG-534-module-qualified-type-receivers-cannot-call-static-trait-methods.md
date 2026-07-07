# BUG-534: Module-qualified type receivers cannot call static trait methods

Severity: LOW
Status: FIXED
Fixed: 2026-07-07
Component: RFC-0042 qualified types, static trait methods, `std/json`

## Problem

RFC-0042 makes plain module imports expose public types under the module
qualifier: `import json` gives the type spelling `json.Json`, while bare `Json`
requires `from json import Json`. Static trait methods did not compose with that
qualified spelling. `Json.from(x)` worked after a `from` import, but the natural
post-RFC-0042 spelling `json.Json.from(x)` failed in the linker as if
`json.Json` were a module-qualified function reference.

## Fix

`type_resolve` now recognizes a method receiver shaped like
`module.Type.method(...)` when `module.Type` names an in-scope type or
constructor, and rewrites that receiver to the existing zero-argument type
receiver representation used by bare static calls. Trait lowering then resolves
the static method through the normal `From(a) for Json` path.

Regression coverage:

- `qualified_type_receiver_static_trait_method_backends_agree` in
  `src/example_tests.rs`
