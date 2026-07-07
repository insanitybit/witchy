# BUG-342: named-field construction of imported records fails

Status: FIXED

Fixed by: parser/linker handling for imported record constructors.

Summary:
- `from rec_lib import FieldInfo` named-field construction was already covered
  by the merged linker strict-record pass.
- This follow-up closes the qualified spelling:
  `rec_lib.FieldInfo(name: "...", ...)`.
- The parser now treats `imported_module.Uppercase(named: ...)` as a record
  literal, so it enters the same merged record-lowering path as unqualified
  named-field construction.

Validation:
- `cargo test -p witchy-syntax named_field_construction_of_imported_record_resolves -- --nocapture`
- `cargo run --quiet -- check /tmp/witchy-bug342-qual.witchy`
- `cargo run --quiet -- run /tmp/witchy-bug342-qual.witchy`
