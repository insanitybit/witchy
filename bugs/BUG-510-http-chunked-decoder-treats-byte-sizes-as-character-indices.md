# BUG-510: HTTP chunked decoder treats byte sizes as character indices

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 28dbc2b
Component: `std/http`, HTTP response parsing, UTF-8 body handling, OAuth/OIDC clients

## Resolution

Current `std/http` decodes chunk framing over `Bytes`, not character-indexed
`String` slices:

- `dechunk(...)` converts the response body to `Bytes`, locates CRLF delimiters
  with `bytes.index_of`, slices payload chunks by byte offsets, then converts the
  recovered payload back with `bytes.to_string_lossy`.
- `dechunk_strict(...)` uses the same byte offsets and returns `Err` for invalid
  chunk sizes, missing delimiters, truncated chunks, or invalid UTF-8.
- `try_parse_response(...)` routes public trust-boundary parsing through the
  strict decoder.

Regression: `http_server_hardening_agrees_on_both_backends`, which includes the
exact well-formed Unicode chunk `2\r\né\r\n0\r\n\r\n` through both
`parse_response` and `try_parse_response`.

Verification:

```sh
CARGO_TARGET_DIR=target-codex cargo test http_server_hardening_agrees_on_both_backends -- --nocapture
```

Observed fixed outputs include:

```text
unicode-chunk=é
unicode-strict=é
```

## Historical Problem

`std/http.parse_response` now decodes `Transfer-Encoding: chunked`, but the
decoder applies each chunk's RFC byte count as a Witchy character index. For
non-ASCII response bodies, the decoded payload can include framing bytes or skip
real body text.

This is a different failure mode from BUG-269's lossy malformed-framing policy:
the repro below is a well-formed chunked response. The chunk size `2` is correct
for the UTF-8 body `é`, but `dechunk` slices two characters instead of two
bytes, so it returns `é\r`.

## Repro

`scratch/repro-http-chunked-unicode-body.witchy`:

```witchy
import http
import string

fn main(console: Console):
    let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\né\r\n0\r\n\r\n"
    let body = http.body(http.parse_response(raw))
    print(console, "bytes=${string.length(body)}")
    print(console, "chars=${string.char_count(body)}")
    print(console, string.replace(body, "\r", "<CR>"))
```

Observed:

```text
$ cargo run --quiet -- check scratch/repro-http-chunked-unicode-body.witchy
scratch/repro-http-chunked-unicode-body.witchy: ok

$ cargo run --quiet -- scratch/repro-http-chunked-unicode-body.witchy
bytes=3
chars=2
é<CR>
```

Expected body: exactly `é` (`bytes=2`, `chars=1`), with no carriage return from
the chunk delimiter.

## Source evidence

- `std/http.witchy:367-391` calls `dechunk` for any response whose
  `Transfer-Encoding` header contains `chunked`.
- `std/http.witchy:408-412` correctly documents chunk sizes as byte counts, but
  then says the implementation is exact for ASCII/UTF-8 text payloads.
- `std/http.witchy:416-435` sets `n = string.char_count(body)` and computes
  `stop = start + sz`, then passes those values to `string.substring`, whose
  indices are character offsets.

## Why this matters

Real HTTP APIs can return UTF-8 JSON, HTML, error bodies, and provider metadata
with chunked transfer encoding. The current parser corrupts a conforming
response body before downstream JSON/OAuth/OIDC code sees it, which makes
`std/http` feel like a narrow ASCII demo parser rather than a dependable
standard-library client.

This also reinforces the broader `String`/`Bytes` split tracked by BUG-456 and
BUG-462: HTTP transfer framing is byte-level protocol work, so doing it through
character-indexed `String` slicing is the wrong abstraction.

## Expected fix

Move chunked transfer decoding to a byte-oriented representation, or add
byte-indexed primitives that make this operation precise. The public response
body can stay `String` for now only after the transfer decoder has produced the
correct byte payload and UTF-8/text policy has been applied deliberately.

## Acceptance

- A response body framed as `2\r\né\r\n0\r\n\r\n` decodes to exactly `é`.
- Chunk sizes count bytes for non-ASCII UTF-8 content, not Unicode scalar
  values.
- Strict malformed-framing behavior from BUG-269 is preserved or improved.
- Interpreter and compiled backend behavior match.
