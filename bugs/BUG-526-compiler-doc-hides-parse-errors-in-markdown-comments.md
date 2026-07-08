# BUG-526: `compiler.doc` hides parse errors in Markdown comments

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on `worktree-wt-15500-1783509731`
Component: `std/compiler`, docs generation, self-hosted compiler APIs, error shape

## Problem

`compiler.doc(name, source)` returns Markdown, so parse/comptime-boundary errors
used to be encoded as `<!-- doc error: ... -->`. That is acceptable as a display
convenience, but it is a poor tooling contract: registries and package managers
need an inspectable error channel instead of scraping presentation text.

## Fix

`compiler.try_doc(name, source) -> Result(String, String)` is now the tooling API.
It returns `Ok(markdown)` for valid source and `Err(message)` for parse or
source-only comptime boundary failures. `compiler.doc` remains compatibility
display sugar and still renders those failures as an HTML comment.

The compiled backend uses the same native implementation through
`compiler.__doc_result_json`, so interpreter and wasm behavior are covered by
`compiler_try_doc_reports_parse_errors_as_result_on_both_backends`.
