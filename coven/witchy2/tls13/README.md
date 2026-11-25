# Witchy standard-library TLS 1.3 in Coven

Coven's mirror, CI and review listeners and outbound HTTPS clients use
[Witchy's TLS standard library](../../../witchyc/docs/wasm-tls.md). The
Coven package directory holds the application-specific connection and
identity adapters and the protocol vectors used to check the library.

The library decodes a complete `ClientHello`, retains the exact transcript
bytes, and encodes `ServerHello`. The strong profile requires P-384
support and `TLS_AES_256_GCM_SHA384`. It records SNI, HTTP/1.1
ALPN, and offers of ECDSA P-384/SHA-384 or RSA-PSS-RSAE/SHA-384.
`p384.accept` uses a `Rand` grant to agree with the peer's validated point
and generate the server random, then encodes the reply in Witchy.
`p384.begin` also hashes the exact Hello transcript and derives the
SHA-384 handshake traffic secrets. The EncryptedExtensions encoder can
acknowledge a routed server name and announce HTTP/1.1.
[TLS 1.3 key shares](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.3.8)

When the first ClientHello supports P-384 without offering its share,
`session.accept` sends one HelloRetryRequest. The second ClientHello
must retain its fixed fields and other extensions. It supplies exactly
one P-384 share and may change zero padding. The transcript uses the
synthetic `message_hash` for ClientHello1 and includes the exact retry.
[TLS 1.3 transcript](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.1)

The RFC 8448 trace profile uses X25519 and `TLS_AES_128_GCM_SHA256`.
Both suites share the ephemeral key schedule, which derives handshake and
application record keys, IVs, and Finished tags in each direction. The HMAC
construction, HKDF labeling, key schedule, and Hello protocol run in Witchy.
The provider supplies SHA-256 and SHA-384.
[TLS 1.3 key schedule](https://www.rfc-editor.org/rfc/rfc9846.html#section-7.1)

The record layer reads and writes both TLS 1.3 AES-GCM suites. Read and write
epochs own their sequence state and start at zero for fresh directional
keys. Install each directional key once and close its epoch when finished.
Witchy validates framing, derives the nonce, tracks sequences, and checks
authenticated content type and padding. The Wasm host provides only the AES
operation through AWS-LC. A failed tag leaves guest output bytes untouched
and closes the read epoch. The write epoch stops at 2²⁴ records to leave
margin for AES-GCM's key usage limit. [TLS record protocol](https://www.rfc-editor.org/rfc/rfc9846.html#section-5)

```sh
grimoire build .
grimoire test .
```

The tests compare the negotiated Hello messages and derived keys with the
published 1-RTT handshake trace. They also check truncated messages,
duplicate extensions, TLS 1.2 refusal, and zero key-agreement output.
Tests also check the strong profile's Hello encoding and refusal of a trace-only
offer. Record tests decrypt the trace's first encrypted handshake flight and
re-encrypt it byte for byte. Other cases cover the next sequence number,
padding, invalid frame headers, malformed authenticated content, and tag failure.
[Handshake trace](https://www.rfc-editor.org/rfc/rfc8448.html#section-3)

The strong suite is checked against independent key, Finished and record
vectors. A loopback OpenSSL client authenticated EncryptedExtensions from
compiled Witchy and refused a record with a changed tag.

`server_auth.encode` produces EncryptedExtensions, a leaf-first certificate
chain, CertificateVerify, and server Finished. Witchy builds the exact
transcript and serializes those messages. A descriptor-bound `TlsSigner`
signs only the TLS 1.3 server context using ECDSA P-384/SHA-384. OpenSSL
validated the server authentication messages on loopback and sent its own
Finished. [TLS authentication messages](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.4)

The socket transport reassembles a bounded ClientHello and fragments the
protected server handshake to fit the supplied record buffers. It leaves
the caller responsible for closing a failed connection.
[TLS record layer](https://www.rfc-editor.org/rfc/rfc9846.html#section-5.1)

`session.accept` begins a fresh exchange on its socket, sends the server
flight, and reassembles the protected client Finished. It compares the
SHA-384 Finished tag through the crypto provider before deriving application
keys. Successful acceptance returns a `SecureSession` that owns the socket
and separate read and write epochs. Call `read` and `write` for protected
application bytes, `shutdown` to send close_notify, and `close` to release
the socket and both epochs. A read of zero denotes an authenticated
close_notify. Bare socket EOF is an error.
[TLS Finished](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.5.3)

The session handles authenticated KeyUpdate messages, including fragments
spanning records, and resets the affected direction's record sequence when
it installs the new key. Peer requests share one pending response, sent
before the next application record. `update_keys` can also initiate a
sending-key rotation and optionally request one from the peer. Each write
epoch reserves its last allowed record for a local rotation.
[TLS KeyUpdate](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.7.3)

A Witchy listener completed an HTTP/1.1 request from OpenSSL on loopback. An independent client exercised a fragmented Finished,
a correctly encrypted record with a wrong Finished tag, and application
content sent before Finished.

Coven's [HTTPS mirror](../mirror/README.md#https-mirror) uses this session to
serve the published TUF tree. The profile uses server certificate
authentication with fresh ephemeral keys. The installed-client witness
verifies this path on loopback.

The client sends a TLS 1.3 ClientHello with DNS SNI, HTTP/1.1 ALPN,
P-384 and AES-256-GCM. It validates ServerHello and the protected server
flight against the exact transcript. Before it sends client Finished, it
verifies the certificate path to caller-supplied DER trust anchors, the
DNS subject alternative name, and the SHA-384 CertificateVerify signature.
The server and client share the application session, with role-specific key
directions. The client can discard server session tickets and handle key
updates on the authenticated connection.
[TLS 1.3 authentication](https://www.rfc-editor.org/rfc/rfc9846.html#section-4.5)
· [TLS service identity](https://www.rfc-editor.org/rfc/rfc9525.html)

An OpenSSL peer with a root, intermediate, and P-384 leaf completed an
HTTPS exchange. An untrusted root, missing intermediate, wrong DNS SAN,
common-name-only certificate, and altered CertificateVerify were refused.
Coven uses this client for CI staging and receipt requests. The runner
provides a snapshot of its configured PEM CA file.
