# BUG-519: `result.unwrap_err` is a defaulting helper

Severity: LOW
Status: FIXED
Verified: 2026-07-07 CODE on branch worktree-wt-bug-519-result-helper
Component: `std/result`, stdlib naming consistency, RFC-0044 error-shape cleanup

## Problem

`std/result.unwrap_err` did not unwrap an error in the usual strict sense. It
accepted a default value and returned that default when the `Result` was `Ok`:

```witchy
pub fn unwrap_err(r: Result(a, e), default: e) -> e:
    match r:
        Ok(_) -> default
        Err(x) -> x
```

That behavior is useful, but the name looked like a strict assertion that the
result is an `Err`.

## Fix

`std/result` now exposes `unwrap_err_or`, the error-side counterpart of
`unwrap_or`, for the defaulting behavior:

```witchy
result.unwrap_err_or(r, default)
```

`unwrap_err` remains as a compatibility alias that delegates to
`unwrap_err_or`, but its docs now explicitly say to prefer `unwrap_err_or` in
new code so the defaulting contract is visible at the call site.

## Acceptance

- The public stdlib docs no longer teach `unwrap_err` as the primary defaulting
  operation under a strict-looking name.
- Existing callers of `unwrap_err(r, default)` keep working.
- The clearer `unwrap_err_or(r, default)` helper is documented.

## Verification

- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo nextest run -E 'test(all_std_modules_type_check) | test(stdlib_docs_are_current)'`
