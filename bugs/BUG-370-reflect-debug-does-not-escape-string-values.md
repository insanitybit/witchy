# BUG-370: `reflect.debug` emitted raw C0 controls that JSON already escapes

Severity: LOW
Status: FIXED
Fixed: 2026-07-07
Component: `std/reflect`, `std/json`, reflective consumers, debug rendering

## Problem

`reflect.debug` already escaped quotes, backslashes, newline, tab, and carriage
return in string values, but every other C0 control was still appended raw.
That was inconsistent with `std/json`, whose encoder renders the full C0 range
as stable string escapes.

The remaining raw controls included NUL, backspace, form feed, ESC, and other
non-printing bytes that can make debug output unstable in terminals and logs.
This mattered because `reflect.debug` is structural text: strings nested inside
records, lists, variants, tuples, sets, or dicts should not smuggle invisible
control bytes into otherwise inspectable output.

## Fix

`std/reflect.escape_string` now mirrors JSON's C0 discipline: after the common
quote, backslash, `\n`, `\t`, and `\r` escapes, any `c < " "` is rendered as
`\b`, `\f`, or `\u00XX`.

Regression coverage checks both interpreter and wasm output for backspace,
form-feed, NUL, and ESC nested inside a reflected record.
