# Standard library for capability-scoped applications

The compiler supplies the named imports below. Operations that touch the
host require the corresponding capability on the call path. The launch
policy controls the actual resource and runtime limits.

| Work | Imports and authority |
| --- | --- |
| File, directory, and socket byte I/O | `fs` takes a file or directory with the requested rights. `net` uses a granted connection, listener, or dial capability. [File APIs](wasm-files.md) · [Network grants](wasm-capabilities.md) |
| Buffered byte streams | `bufio` owns preallocated storage. `bufio_fs` and `bufio_net` borrow the file or socket for each transfer. [Buffer lifetimes and partial writes](wasm-bufio.md) |
| HTTP/1.1 and HTTPS | `http`, `http_framing`, and `https` use a connected socket or established TLS session. Caller-supplied buffers bound head and body storage in the framed readers. `https_fs` streams a bounded response into a staged file or sends an exact-length response body from a readable file. [HTTP framing and compatibility helpers](wasm-http.md) · [TLS, certificates, and HTTPS](wasm-tls.md) |
| HTTP URLs and origins | `url` returns views borrowing the original bytes. The caller provides connection authority and certificate trust. [Canonical origin profile](wasm-url.md) |
| JSON | `json` emits events and `json_tree` builds and serializes an owned tree. Both use the allocator supplied by the caller. [Grammar and bounds](wasm-json.md) |
| Binary encodings | `encoding` appends base64, unpadded base64url or hexadecimal to a `String` and decodes them into owned bytes. An append can leave a partial result if the allocator refuses. Use `decode_base64url_bounded` to cap decoded bytes before allocation and distinguish that limit from invalid input or allocation failure. [Encoding API](../library/wasm-encoding.wy) |
| Crypto and keys | `crypto` computes digests and verifies signatures on public bytes. Signing, secret-backed operations, and random key generation use separate capabilities. [Crypto and provider scope](wasm-capabilities.md#crypto) |
| Arbitrary-precision natural numbers | `big` supplies `BigNat` with in-place word multiplication, scaled addition, checked subtraction, comparison, and copying. Growth uses an explicit allocator. [BigNat API](../library/wasm-big.wy) |
| Time | `time` supplies typed millisecond durations. `Clock.now_ms` reads the host's Unix epoch time. Sleeping or reading the current time needs a clock grant. `parse_utc_seconds` and `append_utc_seconds` convert between whole Unix seconds and canonical UTC text without a clock grant. [Time API](../library/wasm-time.wy) |

For deployment, check the exact [native target coverage](native-backend.md#supported-operations-and-trust-boundary)
or [Wasm host grants](wasm-capabilities.md). Keep network and execution
deadlines and byte limits in the launch policy. An HTTP buffer limit
covers message storage. The runtime limits cover the work and traffic
used to fill it.

A `BigNat` starts at zero. `with_capacity` reserves space in units of 64-bit
limbs, and operations grow that space when needed. Allocation failure leaves
the numeric value unchanged. `subtract` returns false and leaves the value
unchanged if the result would be negative. Use `using` or `discard()` to release
the storage. The pi-digits benchmark represents its signed accumulator as the
difference of two natural numbers.

The UTC text profile is `YYYY-MM-DDTHH:MM:SSZ` through year 9999.
It accepts Gregorian dates and seconds 00–59. Parsing distinguishes a
valid date before 1970 from malformed input. Formatting appends to a
caller-supplied string and can leave a partial result if the allocator refuses.

Database drivers, [TOML](wasm-toml.md), JWT/JWK/OIDC, WebAuthn and its CBOR/COSE formats,
TUF, and Glamour belong to application dependencies. Coven uses SQLite
for private staging and TUF for the public mirror. Start with the
[Witchy registry workflow](../../coven/witchy2/tuf.md) or
[Linux HTTPS deployment](../../coven/deploy/witchy2/README.md).
