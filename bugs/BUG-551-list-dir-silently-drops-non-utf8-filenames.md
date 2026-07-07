# BUG-551: `list(dir)` rejects non-UTF-8 filenames

Severity: MED
Status: FIXED
Fixed: 2026-07-06
Component: `Dir`, runtime filesystem listing, interpreter/compiled parity

## Problem

`list(dir)` silently dropped directory entries whose names were not valid UTF-8.
Both the interpreter and compiled runtime converted each `OsString` with
`into_string().ok()` under `filter_map`, so a real file could disappear from a
program's view with no error.

That violated the capability boundary contract: Witchy exposes directory names
as `String`, so a host filename that cannot be represented as a `String` must be
a loud runtime error rather than invisible data.

## Fix

Both backends now treat directory iteration as all-or-error. A per-entry
`read_dir` error or a non-UTF-8 filename aborts `list(dir)` with a `list failed`
message, instead of dropping that entry and returning a partial list.

Regressions:

- `dir_list_rejects_non_utf8_names` (interpreter)
- `sandbox_dir_list_rejects_non_utf8_names` (compiled source and precompiled wasm)
