# BUG-506: HTTP validators allow NUL/control bytes despite the control-byte contract

Status: FIXED
Verified: 2026-07-08 fixed on master 8af888b
Severity: MED
Component: `std/http`, `std/server`, request/response rendering, header validation

## Resolution

`std/http` now rejects all forbidden C0 control characters and DEL at outbound
HTTP rendering boundaries. Header values allow HTAB but reject other controls;
request-line fields are stricter and reject controls plus spaces/tabs. Server
response rendering uses the same validator before emitting handler-supplied
headers.

Regression: `http_crlf_header_validators_trap_on_both_backends`, including NUL,
SOH, DEL, and server response rendering.

Verification:

```sh
CARGO_TARGET_DIR=target-codex cargo test http_crlf_header_validators_trap_on_both_backends -- --nocapture
```

## Historical Problem

`std/http`'s shared request/response validators say they are the final boundary
before raw HTTP bytes are rendered, and the comments state that a header
value/path/host must never carry a control byte. The implementation only rejects
CR/LF for header values, plus space/tab for request-line fields.

Other C0 controls, including NUL, pass through:

- `http.check_request_field(...)` accepts `"/ok" + string.from_code(0) + "tail"`;
- `http.check_header(...)` accepts `"ok" + string.from_code(0) + "tail"`;
- the client renderer then concatenates those values into request bytes; and
- `server.render(...)` validates handler headers with the same
  `http.check_header(...)`, so response header values have the same gap.

This makes the HTTP stdlib feel half-hardened: the comments and bug history say
the raw string boundary is defended, but the public validator still permits
invalid HTTP control characters that many parsers/proxies treat inconsistently.

## Evidence

Repro file, kept under ignored `scratch/`:

- `scratch/repro-http-control-byte-validators.witchy`

Commands:

```sh
cargo run --quiet -- check scratch/repro-http-control-byte-validators.witchy
cargo run --quiet -- scratch/repro-http-control-byte-validators.witchy
```

Observed:

```text
scratch/repro-http-control-byte-validators.witchy: ok
control-byte-validators-passed
```

The repro calls:

```witchy
let nul = string.from_code(0)
http.check_request_field("request path", "/ok" + nul + "tail")
http.check_header("x-test", "ok" + nul + "tail")
```

Source:

- `std/http.witchy:106-115` calls `check_request_field` before rendering
  request method/path/host into the request line.
- `std/http.witchy:116-125` calls `check_header` before rendering
  caller-supplied request headers.
- `std/http.witchy:295-304` says the validators trap before rendering and that
  header values, paths, and hosts must never carry a control byte.
- `std/http.witchy:327-331` implements `check_field` as CR/LF-only.
- `std/http.witchy:349-351` implements `check_request_field` as CR/LF plus
  space/tab only.
- `std/server.witchy:523-539` routes handler-supplied response headers through
  the same `http.check_header` before raw response rendering.

Generated docs expose this as a public contract:

- `spec/stdlib.md` documents `check_header` as validating a token name and a
  CR/LF-free value.
- `spec/stdlib.md` documents `check_request_field` as rejecting CR/LF,
  space, and tab, but the source-level HTTP boundary prose is stronger: no
  control bytes before rendering.

## Why this matters

This is distinct from adjacent HTTP bugs:

- `BUG-364` is fixed for token-breaking request-line spaces/tabs.
- `BUG-393` tracks client-owned header conflicts, currently duplicate `Host`.
- `BUG-358` is fixed for server-owned framing headers.
- `BUG-438` tracks inbound malformed request-line parsing.
- `BUG-497` tracks inbound transfer-coding request bodies.

`BUG-506` is the shared outbound validator gap: even after CR/LF, whitespace,
and framing fixes, the supposed checked HTTP string boundary still permits
invalid control bytes into request/response wire text.

For a public release, this is the kind of small core-library mismatch that makes
the stdlib look hand-hardened case by case instead of designed around a clear
HTTP token/value contract.

## Expected fix

Define one explicit HTTP character policy and apply it consistently:

- reject C0 controls and DEL in request-line fields;
- reject C0 controls except possibly HTAB in header values, depending on the
  chosen RFC 7230/9110 compatibility stance;
- keep header names on the existing token grammar;
- consider percent-encoding or URL-builder guidance for request targets rather
  than accepting raw controls; and
- update source/generated docs so `check_field`, `check_header`, and
  `check_request_field` describe the same policy they enforce.

Acceptance should include direct tests for NUL and another non-CR/LF control
byte in:

- client request path/host/method;
- caller-supplied request header value; and
- server-rendered response header value.
