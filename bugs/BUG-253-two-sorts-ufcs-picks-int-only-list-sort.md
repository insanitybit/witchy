# BUG-253: two public generic sort implementations remain split across `list` and `cmp`

Status: FIXED
Severity: MED
Fixed: 2026-07-07 (`fix/delete-cmp-list-duplicates`)

## Resolution

Deleted `cmp.sort`. `list.sort` is the sole public generic `List(a) where a:
Ord` sort and remains the method-form owner beside `list.sort_by`.

The generated stdlib reference and book/spec prose no longer present two
independent sort homes.

Validation:
- `cargo run --quiet -- check std/cmp.witchy`
- `cargo test stdlib_docs_are_current -- --nocapture`
- `cargo test all_std_modules_type_check -- --nocapture`
