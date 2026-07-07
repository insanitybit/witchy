# BUG-178: Comptime-generated public APIs are invisible to footprint gates and docs

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Verified: 2026-07-07 CODE on branch `worktree-wt-28523-1783448177`
Component: `comptime`, capability footprint gates, grant checking, generated API docs

## Problem

The language and book say `comptime:` output is appended before type checking
and footprint analysis, and that generated code is analyzed like handwritten
code. The broken state parsed the original source and called
`capabilities::analyze` or `doc::render` without expanding `comptime:` blocks
first. A public function emitted by `comptime:` was therefore missing from
release-facing introspection.

This was more severe than ordinary docs incompleteness: a supply-chain gate
could accept generated public authority growth that it rejected when
handwritten.

## Resolution

- `witchy caps`, `caps-diff`, and `grants-check` type-check and analyze the
  expanded entry module.
- `witchy doc` parses, expands `comptime:`, and renders the expanded AST, so a
  generated `pub fn` appears in API documentation.
- `compiler.footprint`, `compiler.diff`, and `compiler.doc` are source-string
  native intrinsics in `witchy-runtime`, which cannot call the expander without
  introducing a crate cycle today. They now reject `comptime:` source strings
  explicitly instead of returning a false footprint or partial docs.
- `std/compiler.witchy` documents that boundary.

## Regression Coverage

- `caps_counts_comptime_emitted_apis`
- `doc_counts_comptime_emitted_apis`
- `native_compiler_intrinsics_reject_comptime_source_strings`
