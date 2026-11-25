# HTTPS

Coven uses this TLS implementation for the public TUF mirror and its CI
and review HTTPS endpoints. The mirror and installed Grimoire workflow
are exercised together by the [Coven HTTPS witness](../../coven/witchy2/tuf.md#verify-an-installed-client-against-the-https-mirror).

## Client

Use `import tls` to establish an authenticated TLS 1.3 session and
`import https` to exchange HTTP/1.1 requests on it. The client profile uses
P-384 ephemeral key agreement, AES-256-GCM and ECDSA P-384 server
authentication. The call consumes the supplied socket and closes it on
handshake failure.

The program supplies a socket from `Net`, a `Rand` grant, the DNS name to
verify, and explicit trust anchors. Give `tls.connect` Unix seconds from a
trusted clock. `tls_trust_pem.read_trust_bundle(file, allocator)` consumes a
`File<Read>`, reads up to 512 KiB of UTF-8 PEM, and returns length-prefixed
DER certificates. The caller selects the file and the trust anchors it
contains. To verify the exact PEM bytes first, use
`tls_trust_pem.read_trust_pem(file, allocator)`. It applies the same size
limit and returns the original nonempty UTF-8 bytes in a `String`.
Pass its span to `tls_trust_pem.trust_bundle` after checking the pin.
The example opens a PEM root through a read-only directory grant
and uses a `Clock` grant for certificate dates. Network grants apply
to the dialed endpoint.

Use `https_client.connect` when starting from an HTTP origin parsed by
`url.parse_origin`. It requires HTTPS, uses the origin's endpoint for the
scoped `Net` connection, and verifies TLS against that same origin's host
using the supplied clock, randomness, and DER trust anchors. For the
default HTTPS port it dials `host:443`; the origin's HTTP authority
remains `host`. The function takes caller-owned TLS workspaces and
returns separate origin, TCP, and handshake errors. Coven's callers reserve
72,704 bytes for the transcript, 16,385 for plaintext, and 16,406 for the
wire record. Close a returned session after the exchange.

The client verifies the certificate chain and DNS identity, checks the
server's signature over the TLS transcript, and checks its Finished
message before returning the session. The HTTPS wrapper takes caller-owned
buffers for the authenticated response. Close the session after the
exchange.

## HTTP/1.1 over a TLS session

`https.get`, `https.head`, and `https.post` send one request on a fresh authenticated
session. Pass the authority for that session and the request path beginning
with `/`. `post` sends a byte slice with an explicit Content-Type and
Content-Length. The reply contains the status and the used lengths of
the caller's head and body buffers. `head` takes only a response head
workspace and returns a zero body length.

For a server, call `https.read_request` after accepting TLS. Pass the
configured HTTP authority, even when a TCP forwarder accepted the
public connection. This checks Host and fills the caller's head and body
buffers. Use `http.request` on the used head prefix to inspect the method,
target, and headers.

`https.write_response` emits a final response with Content-Length and
sends TLS `close_notify`. Use `write_response_with_headers` for validated
extra headers. Write one response, then close the session. Close it on
any read or write failure and use received buffers only on success.

The caller supplies separate TLS plaintext and wire workspaces for all
these operations. The server also supplies a chunk-line buffer.
The client uses a 1,024-byte chunk-line buffer internally. See
[HTTP message framing](wasm-http.md#bounded-message-framing) for the
framing rules and buffer limits.

### Stream a bounded identity response

Use `https.read_identity_prefix` to read the head and any body bytes in
the same record. Pass the head to `http_framing.identity_response_head`
and validate the status and headers for your application. Initialize
`https.identity_body` with the response framing, the number of buffered
body bytes, and `IdentityAtMost(n)` or `IdentityExactly(n)`. A successful
initialization accounts for those buffered bytes before they are consumed.

Call `https.read_identity_chunk` with the TLS session and the caller's
workspaces. Each successful positive count identifies the bytes available
in the plaintext workspace. Zero marks the end of the response. Discard
a partial result on any error, and publish a streamed file only after
success. `RequireIdentityClose` waits for authenticated TLS closure
after a fixed-length body. `StopAtIdentityLength` accepts a complete
fixed-length body and lets the caller close the one-shot connection.
Close-delimited bodies always require authenticated TLS closure.
The state allocates no buffer and uses the session's existing grants
and deadline.

`https_fs.copy_identity_body` streams the same bounded response into a
caller-supplied `&File<Write>`. Pass the validated framing, the body bytes
already read with the head, and the desired length and closure policy.
The adapter uses the TLS workspaces supplied by the caller and preserves
framing, transport and file-write errors. It leaves the file staged. On
success, call `finish()` when ready to publish. On failure, call `abandon()`
to discard a partial write. Coven's TUF downloader uses this adapter.

`https_fs.send_file_body` sends exactly the requested number of bytes
from a borrowed `File<Read>` through an established TLS session. The
caller supplies the file, plaintext and wire buffers. Choose a bounded,
stable length before writing the response head. Wasmtime's file
snapshot supplies that length through `snapshot_remaining()`.
For HEAD, send the same response head and skip the file body.
If the file ends early, the buffer is empty for a nonempty body, or
TLS writing fails, close the exchange. Some bytes may already have
been sent. Coven's mirror uses this adapter for TUF GET responses.

## Run the client example

[The loopback example](../examples/tls-loopback.wy) connects to a local peer
at `https://localhost:19473` through `https_client.connect`. Use a TLS 1.3
peer that negotiates P-384 and
`http/1.1` through ALPN. Give it an ECDSA P-384 certificate with the
`localhost` DNS SAN and put the issuer in `roots/root.pem`. Set `roots`
to the directory's absolute path.

```sh
witchyc build examples/tls-loopback.wy -o client.wasm --heap-pages=6
witchy-wasmtime-host client.wasm --output --allocator --rand --clock \
  --dir="$roots" --net=localhost:19473 --memory-pages=64 \
  --module-bytes=262144 --fuel=100000000
```

On macOS ARM64, build with a compiler configured with the pinned AWS-LC
provider. The same source uses the native broker for its file and
socket grants and the same certificate policy as Wasmtime.

```sh
witchyc build examples/tls-loopback.wy -o client --target=macos-arm64
./client --output --allocator --rand --clock \
  --dir="$roots" --net=localhost:19473
```

Use `import key_agreement` to create an opaque `P384Key` from a `Rand`
grant. A key must be consumed by one agreement or closed. The provider
holds at most sixteen unfinished keys per instance. Public certificate
verification in `import certificates` takes bounded DER bundles, a DNS
name, verification time and signed bytes.

[TLS record protocol](https://www.rfc-editor.org/rfc/rfc8446.html#section-5)

## Serve HTTPS on Wasmtime

[The echo server](../examples/https-server.wy) serves HTTP/1.1 over TLS
1.3 using P-384 key agreement, ECDSA P-384 server authentication, and
AES-256-GCM. Each guest instance receives one accepted socket. The
server takes a read-only directory with `chain.bin`, a sequence of
big-endian 24-bit lengths followed by their DER certificates. Put the
leaf first, followed by any intermediates. Use a leaf whose DNS SAN
contains `localhost` for this example.

The `TlsSigner` grant loads a PKCS#8 DER P-384 private key from an
inherited file descriptor. Its only operation signs the TLS 1.3 server
CertificateVerify context with a 48-byte transcript digest. The host
issues a fresh handle in each connection instance. Keep the key file
outside the guest's certificate directory.

```sh
witchyc build examples/https-server.wy -o echo.wasm --heap-pages=8
witchy-wasmtime-host echo.wasm --allocator --rand \
  --listen=127.0.0.1:19473 --instance-per-connection --connections=3 \
  --dir="$certs" --no-links --deadline-ms=10000 --fuel=100000000 \
  --memory-pages=64 --module-bytes=262144 --net-bytes=65536 \
  --secret-slot=issuer:sign/tls13/server-p384:fd:3 3<"$server_key"
```

Set `certs` to the absolute path of the certificate directory and
`server_key` to the path of the private key. Use `--listen=127.0.0.1:0`
to have the host pick a port and print the bound address to stderr.

## Serve HTTPS on macOS ARM64

Build the same server with a compiler configured with the pinned AWS-LC
provider. The launcher reads the inherited descriptor and imports the
key before accepting connections. Each sandboxed worker can use the
signing handle for its connection. The key's source file is protected
by the native broker's descriptor identity checks. Launches with a
`TlsSigner` require read-only file and directory grants and exclude
`Exec`.

```sh
witchyc build examples/https-server.wy -o echo --target=macos-arm64
./echo --allocator --rand --dir="$certs" \
  --listen=127.0.0.1:19473 --instance-per-connection --connections=3 \
  --deadline-ms=10000 --net-bytes=65536 \
  --secret-slot=issuer:sign/tls13/server-p384:fd:3 3<"$server_key"
```

An embedded launch manifest can set the same grants. Add the listener,
certificate directory, randomness and limits to the manifest, and bind
the signing slot with this entry.

```toml
[secrets]
issuer = { use = "sign/tls13/server-p384", from = "fd:3" }
```

Build with `--manifest=server.grants.toml` and run
`./echo 3<"$server_key"`. An ephemeral listener writes its bound
address to stderr on both runtimes.
