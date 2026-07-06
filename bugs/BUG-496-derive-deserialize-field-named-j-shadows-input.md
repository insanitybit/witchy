# BUG-496: `derive(Deserialize)` reused source field names as generated locals

- **Severity:** MED
- **Status:** FIXED
- **Component:** `std/meta`, `derive(Deserialize)`, generated-source hygiene, JSON reconstruction

## Problem

`std/meta.derive_deserialize` generated `from_json(j: json.Json)` and then emitted
one `let <field-name> = ...` per record field. Ordinary source field names could
therefore collide with generator helpers or constructor names:

- a field named `j` shadowed the input JSON object before later fields decoded
- a field named `Ok` emitted `let Ok = ...`, which parsed as a refutable
  constructor pattern rather than a local binding

That made valid record field names order-sensitive and exposed generated code in
diagnostics.

## Resolution

The generated `from_json` method now uses a private generated input name
(`__j`) and binds decoded fields as positional temporaries (`__field0`,
`__field1`, ...). Source field names are used only as JSON object keys, then the
record is constructed positionally from the generated temporaries.

The regression
`derive_deserialize_field_names_are_hygienic_on_both_backends` covers fields
named `j`, `Ok`, `Err`, `Some`, and `None`, plus a later nested
`Option(List(Option(Int)))` field to prove subsequent decoders still read from
the original JSON object.

## Verification

- `cargo test derive_deserialize_field_names_are_hygienic_on_both_backends -- --nocapture`
- `NEXTEST_TEST_THREADS=2 ./scripts/check.sh`

