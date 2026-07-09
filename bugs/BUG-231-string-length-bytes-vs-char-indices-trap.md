# BUG-231: `string.length` (bytes) feeds char-indexed APIs throughout std/projects — silent truncation on non-ASCII

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 8af888b
Component: `std/string` unit model, std/projects call sites, RFC-0049 naming

## Resolution

The release-facing source-wide bug described below has been fixed on current
master:

- `projects/coven-web/src/coven_web.witchy` now uses
  `string.char_count(name) + 1` before dropping a cookie prefix.
- `projects/coven/src/coven_footprint.witchy`, `projects/glamour`, `std/regex`,
  `std/http`, `std/server`, and the string helpers now use `string.drop(...)` or
  `string.char_count(...)` for char-indexed tail slices instead of composing
  `string.length(...)` byte counts into char-indexed APIs.
- The remaining `string.length(...)` arithmetic found by source scan is an
  indentation-width calculation in `projects/docs/src/docs.witchy`, not a string
  slice index.

Regression: `string_tail_slices_use_character_counts_on_both_backends`.

Verification:

```sh
CARGO_TARGET_DIR=target-codex cargo test string_tail_slices_use_character_counts_on_both_backends -- --nocapture
rg -n "substring\([^\n]*string\.length|string\.length\([^\n]*\) \+|string\.length\([^\n]*\) -|drop\([^\n]*string\.length|take\([^\n]*string\.length" std projects examples -g '*.witchy'
```

## Historical Problem

`std/string` mixes two index units in one API surface: `length` counts
**UTF-8 bytes** while every index-consuming function (`substring`,
`index_of`, `char_at`, `take`, `drop`, `pad_*`) counts **Unicode scalars**.
Each function documents its unit — but the *composition* is the trap, and
std's own code walks into it. The idiom `substring(s, i,
string.length(s))` appears ~20 times across `std/` and `projects/`
(exec, regex, time, coven-web, coven, docs, glamour, markdown), and
`substring(kv, string.length(name) + 1, …)` (coven-web `cookie_value`)
uses a byte count as a character *start* index.

For ASCII inputs the two units coincide, so everything passes today. With
any non-ASCII content the arithmetic silently shifts or truncates —
`substring` clamps out-of-range indices (by design), so there's no error,
just wrong output:

```console
$ cat scratch/round3-audit/u3.witchy
fn main(console: Console):
    let kv = "naïve=x"
    let name = "naïve"
    let out = string.substring(kv, string.length(name) + 1, string.length(kv))
    print(console, out)

$ witchy scratch/round3-audit/u3.witchy
                     # <- EMPTY line; the value "x" is lost
```

(`length("naïve") = 6` bytes; char-start 7 > char-end == clamped, empty
result. Both backends agree — parity holds; the output is wrong on both.)
That is exactly the coven-web cookie-parsing shape (`cookie_value`,
coven_web.witchy:429): a cookie name with any multibyte character makes its
value unreadable, silently.

`substring(text, prev, string.length(text))` (regex.witchy:58,68) drops
trailing characters whenever the *scanned* text contains multibyte chars,
because the byte length exceeds the char count only in ways clamping
hides... in the other direction: byte length ≥ char count, so the end index
over-covers and clamps safely there — the *start*-index uses (coven-web,
coven_footprint:64, markdown:170) are the corrupting ones, plus any
`length`-derived arithmetic stored and reused as a char offset.

## Current source status

Source refresh on 2026-07-06 confirms the bug is still current. `std/string`
still documents `length` as byte-counted (`std/string.witchy:12-15`) while
`substring`, `char_count`, `chars`, `take`, `drop`, and the padding helpers use
Unicode-scalar indexes. Current call sites still compose byte lengths with
char-indexed slicing:

- `projects/coven-web/src/coven_web.witchy:429` uses
  `string.length(name) + 1` as the start index and `string.length(kv)` as the
  end index for cookie values.
- `projects/coven/src/coven_footprint.witchy:73` and `:79` use
  `string.length(marker)`, `string.length(s)`, and `string.length(after)` in
  substring arithmetic.
- `projects/glamour/src/markdown.witchy:28`, `:35`, `:49`, `:162`, `:165`,
  `:170`, `:176`, `:183`, `:191`, and `:195` use `string.length(...)` as
  substring ends while parsing Markdown.
- `std/regex.witchy:58` and `:68` use the same idiom for trailing fragments.

No runtime repro was rerun in this pass because another agent is actively
repairing the worktree/toolchain; the current finding is a source-level refresh
of the earlier repro.

## Why this matters

- RFC-0044/0049 fixed the *error-shape* and *naming* lexicons; the index
  *unit* lexicon is the same class of consistency debt, and it's currently
  "every function for itself".
- A release-visible failure: cookies, query params, markdown links with
  non-ASCII text — web-facing code paths, in the flagship dogfood project.
- The pattern `substring(s, i, string.length(s))` exists *because* there is
  no "rest of the string" API; users reach for the only length in sight.

## Fix direction

1. **Audit + fix the call sites**: byte-`length` used in char-index
   arithmetic. Mechanical: `substring(s, i, string.length(s))` →
   `substring(s, i, string.char_count(s))` — or better, add
   `string.from(s, i)` / make `substring` accept an end-omitted form (a
   defaulted parameter is now expressible per RFC-0056 — but see BUG-206
   first), so "to the end" stops being spelled with a length at all.
2. **Consider the naming fix** (RFC-0049 spirit): `length` returning bytes
   is a Go-ism the rest of the API doesn't share; `byte_count` next to
   `char_count` with `length` retired (break-don't-deprecate) would make
   the unit visible at every call site. If `length` stays, its doc-comment
   warning is not enough — nothing flags the composition.
3. A differential test with non-ASCII cookie names/values through
   coven-web's `cookie_value`, and a book/example note on the unit model.
