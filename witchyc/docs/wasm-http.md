# HTTP: `import http`

```
fn main(console: Console<Write>, net: Net, allocator: Allocator):
    match http.get(&net, &allocator, "127.0.0.1:8080", "127.0.0.1", "/hello.txt"):
        Ok(response):
            console.print_int(response.status)        // 200
            response.close()
        Err(Connect(NotGranted)):
            ...
```

An HTTP/1.1 client written in Witchy over `Net` (library/wasm-http.wy): one request per
connection and `Connection: close`. The client decodes one bounded response
into its status and UTF-8 body. It reaches only what the run grants: `--net=127.0.0.1:8080`, and an endpoint
outside the grant answers `Err(Connect(NotGranted))`. `Net.only` narrows what a library
given the `Net` can dial.

`wasm-caps` shows where the authority goes: `get` holds `Net` and memory, and `parse`
memory only, so the code that reads untrusted bytes cannot dial anything.

- `http.parse(&text, &allocator)` splits already-buffered text at the header
  terminator and returns the raw body bytes as text. Use `http.get` or
  `http_framing` for message framing and transfer decoding.
- `http.head(text.span())` reads one in place: `.{status, headers, body}`, the header lines
  and the body as `Span`s of the text, nothing copied, the text lent while they are in use;
  `http.header(parts.headers, "content-type")` finds a header by name in any case, without the
  spaces around its value (`http/head.wy`). Needs no memory, so it holds no capability at all.
- For a server, `http.request_on(&socket, Span.of(bytes))` reads a request head from bytes the
  socket gave it: `.{method, target, headers, body}` in place, the target already refused for
  spaces and control bytes; `http.has_head(...)` says whether the blank line has arrived
  (`packages/httpd/` uses both). For fragmented input, pass the growing prefix and the
  previous incomplete prefix length to `http.head_end`. It returns zero while incomplete
  and the offset after the first CRLF CRLF when complete, even if the same read contains
  body bytes. Keep the prefix in a bounded buffer. `request_on` checks the listener's
  granted address: the socket says which listener accepted it (`socket.accepted_on`, the grant's
  `host:port`, `[::1]:8080` for IPv6), and the request must name exactly that in exactly one
  `Host` header. `Err(400)`: not a request, no `Host`, or two. `Err(421)`: any other name,
  `localhost` included, which is what a browser sends after DNS rebinding. The check alone is
  `http.host_status(headers, authority)` (`http/host.wy`).
- `http.request(Span.of(bytes))` is the parser alone (`http/request.wy`), for bytes that did not
  come from a granted listener. `Span.of(bytes)` makes a Span of bytes that did not come from a
  String; only `to_string` assumes UTF-8, and it checks.
- A path or host with a space or a control byte (which could add lines to the request) is
  refused before anything is sent, and a path starts with `/` (`http/injection.wy`).

Use [`import url`](wasm-url.md) to parse a borrowed canonical origin or
absolute HTTP URL before choosing a network connection.

Use `import https` to exchange HTTP/1.1 messages on an authenticated TLS
session. See [the TLS client](wasm-tls.md) for the grants and loopback example.

## `Fetch`: one endpoint, GET only

```
match http.fetcher(&net, "127.0.0.1:8080", "127.0.0.1"):
    Ok(fetch):
        status_of(&fetch, &allocator, "/hello.txt")    // fn status_of(fetch: &Fetch, ...)
```

`Fetch` is a capability the library defines (book: the `Fetch` grant), holding a `Net`
narrowed by `only` to one endpoint. Code given a `Fetch` can GET paths there and nothing
else: the `Net` inside is reached only in the library's file
(`http/fetch-escape.refused.wy`), and only the library builds one
(`http/fetch-forge.refused.wy`). `wasm-caps` reports `status_of: Allocator, Net (in Fetch)`.
Run by hand against a local server: 200, then 404.

The convenience `http.get` and `Fetch` helpers decode fixed-length,
chunked, and close-delimited replies with a 16 KiB head limit and a 16 MiB
decoded body limit and 256-byte chunk and trailer line workspaces. They
return as soon as a framed reply is complete. The host's fuel, deadline,
and network budgets also apply. A malformed, truncated, or over-limit
response returns `Malformed`.
A body that fails UTF-8 validation returns `Connect(NotText)`. Use the
socket API below for binary bodies or caller-defined buffer limits.

## Bounded HTTP on sockets

Open a connection with a granted `Net`, write a validated GET with
`http.send_get(&socket, host, path)`, and read the response with
`http_framing.read_response(&socket, head, body, line, input)`. On an
accepted socket use `http_framing.read_request(&socket, head, body, line,
input)`. It requires one `Host` matching the listener's granted
authority. Use `http.response_head` or `http.response_head_with_headers`
to encode a response head and write it and its body to the socket.
Use `ResponseHeaders.add_text` for literal field names and values. It
applies the same field and framing checks as `ResponseHeaders.add`.
For a TLS listener, call `http.https_host_status(head.headers, authority)`
with the configured `host:port`. It accepts an omitted port only when
the configured port is 443. The plain HTTP `http.host_status` uses the
same singleton check with port 80. The helpers return 0 for a matching
authority, 400 for a missing or repeated Host, and 421 for a mismatch.

Use `http.response_head_for_version` with `ResponseHttp10` when an
HTTP/1.0 compatibility endpoint needs the same fixed-length profile.

Pass mutable byte slices for the head, decoded body, chunk-line workspace,
and nonempty transport scratch. The reader allocates no buffers of its own.
It returns the valid head and body lengths, plus the status for a response.
`SocketReadFailed` carries a transport error and `SocketMessageRefused`
carries a framing error. Discard the buffers on error and close the
connection after the exchange. Use the runtime's network and deadline limits
and Wasm fuel to bound traffic and execution in addition to the storage limits.

The loopback check compiles Witchy client and server fixtures and runs them
with the installed Wasmtime host. Set `WITCHYC`, `WITCHY_WASMTIME_HOST`
and the usual proof environment, then run from the monorepo root:

```sh
python3 witchyc/tests/http_sockets/run.py
```

On macOS ARM64, add `--native` to exercise the same fixtures on the
native compiler and socket runtime.

## Bounded message framing

For a HEAD exchange over a connected socket, encode the request with
`HeadRequest` and use `http_framing.read_head_response(&socket, head, input)`.
For an authenticated TLS session, use
`https.head(&mut session, allocator, authority, path, head, plaintext, wire)`.
Pass the authority and target as borrowed spans, for example the authority
returned by `HttpOrigin.authority()`. Close the connection after the exchange.
The returned body length is zero.

Use `import http_framing` when reading a request or response in pieces.
The decoder owns framing state. The caller owns the transport and supplies
the same mutable head, body, and chunk-line buffers on every call.

1. Start with `request_decoder()` or `response_decoder()`. Feed each read
   to `push_bytes`, including the configured authority for requests.
2. Stop when `complete()` is true. Call `finish()` when the transport
   reports EOF first. For TLS, only an authenticated `close_notify`
   counts as EOF.
3. After success, use only the prefixes given by `head_len()` and
   `body_len()`. A response also reports `status()`. Discard the
   buffers' contents on error.

Use `head_response_decoder()` for a response to HEAD. It handles
informational responses and completes at the final head delimiter.
Pass empty body and chunk-line slices. Content-Length describes the
representation and does not consume body capacity. Normal header checks
still apply, and the decoder refuses any bytes after the final head in
the same input.
[RFC 9112 message body length](https://www.rfc-editor.org/rfc/rfc9112.html#section-6.3)

To stream a response, use `ResponseDecoder.push_body_chunk` instead of
`push_bytes`. Supply the same head and chunk-line workspaces and the same
total body limit on each call. The output scratch can be reused and should
be at least as large as the transport input chunk. Only the returned
prefix contains decoded bytes from that call. Keep partial output private
until the decoder completes or successfully finishes at EOF, then commit
the output. Discard it on any later read or framing error.

Requests require HTTP/1.1, a slash-prefixed target, and one Host matching
the configured authority. A request with no body framing has an empty body.
Responses support content length, chunked transfer coding, and
close-delimited bodies where allowed. The decoders reject conflicting
transfer coding and content length, repeated length fields, bodies that
exceed the caller's buffer, and bytes following the framed message in
the same read. The API handles one exchange per connection. See
[HTTPS client and server helpers](wasm-tls.md#http11-over-a-tls-session).

For a route that accepts only a request head, bound the received bytes
and call `http_framing.parse_request_head(bytes)`. It requires an HTTP/1.1
request line and a slash-prefixed target. Check the returned authority
fields and call `http_framing.request_without_body(head)`. This checks field-line syntax
and refuses buffered bytes after the head, Content-Length (including zero),
Transfer-Encoding, and Expect. The caller still selects allowed methods
and paths. Both Coven mirrors use this check after their authority checks.
[RFC 9112 field syntax](https://www.rfc-editor.org/rfc/rfc9112.html#section-5)

Use `http.request_headers` and `http.request_head` to stage and serialize
one HTTP/1.1 GET, HEAD, or fixed-length POST for either a socket or a TLS
session. The encoder owns Host, Connection and Content-Length. It
refuses framing overrides, Expect, malformed fields and request heads
over 32 KiB. A request can have one Content-Type field. Duplicate field
names are compared case-insensitively for this check. The caller chooses
an already authorized origin and sends exactly the declared POST body.
[RFC 9110 Content-Type](https://www.rfc-editor.org/rfc/rfc9110.html#section-8.3)

For a server that needs to validate routing or authorization before
allocating a body, use `https.read_request_prefix`. Pass a head limit that
includes the final CRLF CRLF. The prefix buffer must also fit any body
bytes delivered in the same TLS read. The result gives the first delimiter
offset and total bytes received. After validating the request framing and
declared length, `https.read_fixed_request_body` fills a slice of exactly
that length. Pass the body suffix from the prefix as its initial bytes.
It refuses early EOF and later TLS reads that exceed the remaining space.
Use the body only after the read succeeds.

For clients that stream identity content, `https.read_identity_prefix`
reads from an authenticated TLS session into caller-provided storage. It
returns the first CRLF CRLF offset and the number of bytes received,
including any body bytes in the last TLS record. It requires a complete
head within 16 KiB and reports transport errors separately from an
incomplete or oversized head. The prefix buffer must also fit any body
bytes received with that head. Only use the prefix after validating it.

`http_framing.identity_response_head(bytes, fence)` validates that prefix
and checks a complete HTTP/1.1 head with at most 64 fields. The profile accepts status 200
and 4xx/5xx, with absent or one valid Content-Length. It refuses transfer
or content coding, duplicated length fields, and already-buffered bytes
beyond a fixed length. The result includes the parsed status, body offset,
fields, and length framing. The caller supplies the destination and the
body byte budget. A close-delimited TLS response ends only at an
authenticated closure alert. Coven's TUF downloader uses this path to
stream into a file, and its CI client requires the fixed-length case.
[RFC 9112 message length and TLS closure](https://www.rfc-editor.org/rfc/rfc9112.html#section-6.3)
