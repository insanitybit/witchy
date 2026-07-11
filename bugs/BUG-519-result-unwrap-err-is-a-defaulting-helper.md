# BUG-519: `result.unwrap_err` is a defaulting helper

Severity: LOW
Status: FIXED
Verified: 2026-07-11 CODE on branch cleanup/result-unwrap-err-alias
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

The misleading `unwrap_err` spelling is deleted. This is a pre-0.1 clean break:
keeping a zero-call-site compatibility alias would contradict RFC-0044's
explicit delete-not-deprecate rule and leave two names for one operation.

## Acceptance

- The public stdlib exposes no strict-looking `unwrap_err` defaulting helper.
- The clearer `unwrap_err_or(r, default)` helper is documented.
- There were no Witchy call sites to migrate.

## Verification

- `CARGO_TARGET_DIR=target-codex CARGO_BUILD_JOBS=1 cargo nextest run -E 'test(all_std_modules_type_check) | test(stdlib_docs_are_current)'`
