# BUG-483: `json.get_path` cannot address keys containing dots

Status: FIXED
Verified: 2026-07-09 fixed on branch fix/json-get-in
Severity: LOW

## Summary

`std/json.get_path` remains a dotted object-key lookup helper, but
`std/json.get_in(j, segments)` now provides the exact segment-list alternative.
JSON object keys are arbitrary strings, so keys containing `.` or empty-string
keys are reachable through `get_in` without inventing an escaping grammar for
the convenience string helper.

## Evidence

- `std/json.witchy` exposes `get(j, key)` as exact object-key lookup.
- `std/json.witchy` now exposes `get_in(j, segments)` for exact nested lookup.
- `std/json.witchy` keeps `get_path(j, path)` as a dotted convenience wrapper
  over `get_in(j, string.split(path, "."))` and documents that `get_in` is the
  API for literal dots.
- `spec/stdlib.md` publishes both APIs after regeneration from stdlib comments.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-json cargo test json_get_in_reaches_literal_dot_keys_on_both_backends -- --nocapture
```

For example, `JsonObject([("a.b", JsonInt(1))])` can be queried with
`json.get_in(obj, ["a.b"])`; `json.get_path(obj, "a.b")` intentionally still
looks for nested key `a` then key `b`.

## Impact

This was a small polish issue, but it made the accessor surface less powerful
than it appeared. First-party protocol data and metadata formats can legally use
literal dots in object keys; the release-facing path helper surface now supports
that shape explicitly.

## Suggested Fix

Fixed by adding `get_in(j, segments: List(String))`, leaving `get_path` as a
convenience for simple dotted paths, and documenting the split.

## Acceptance Criteria

- There is a supported way to retrieve a literal-dot key through the nested
  accessor surface.
- `get_path(resp, "user.name")` keeps its existing behavior for simple keys.
- The generated stdlib docs state the supported path grammar or limitation.
