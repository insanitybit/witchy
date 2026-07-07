# BUG-034: `derive(Deserialize)` ignored prelude import rules

Severity: MED
Status: FIXED
Fixed: 2026-07-06 (`fix/derive-deserialize-prelude`)
Component: `derive(Deserialize)`, prelude visibility, std docs

## Resolution

`derive(Deserialize)` now follows the same prelude visibility rules as
handwritten Witchy source. The derive still requires explicit `import json`,
because generated `from_json` code calls JSON decoders from a non-prelude module.
It no longer requires explicit `import result` or `import option`:

- `Result`, `Ok`, and `Err` are prelude names.
- `Option`, `Some`, and `None` are prelude names.
- Option fields at any nesting depth decode without redundant imports.

The stale source comments and docs were updated to make the rule singular:
import `json` for deserialize derive; do not import prelude data modules just to
make generated code see their constructors.

## Validation

Regressions:

- `derive_deserialize_nested_option_backends_agree`
- `derive_deserialize_field_names_are_hygienic_on_both_backends`

Focused checks:

- `cargo test derive_deserialize_nested_option_backends_agree -- --nocapture`
- `cargo test derive_deserialize_field_names_are_hygienic_on_both_backends -- --nocapture`
- `cargo test -p witchy-syntax`
- command-level `witchy check` repro with `import json` only
