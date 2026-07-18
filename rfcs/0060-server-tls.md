---
rfc: 0060
title: Server-side TLS — HTTPS serving with capability-guarded keys
status: implemented
created: 2026-07-04
tracking: "serve_tls/serve_tls_n both backends; use-only secrets; rustls"
---

# RFC-0060: Server-side TLS — HTTPS serving with capability-guarded keys

The host-side listener path is implemented in
[`crates/witchy-interp/src/interpreter.rs`](../crates/witchy-interp/src/interpreter.rs),
with end-to-end TLS coverage in [`tests/e2e.rs`](../tests/e2e.rs).

## Summary

Add TLS serving to `std/server`: `serve_tls` (and pool variants) accepts a
certificate chain plus a private key held as a `Secret`, with the handshake
performed host-side so the key bytes are never addressable by the program that
serves with them. A small extension to the secrets model — a **use-only**
grant flag that makes a secret unrevealable — makes that guarantee real rather
than rhetorical.

## Motivation

witchy has an HTTPS *client* (RFC-0009: the `tls:host:port` dial scheme) but
no way to *serve* TLS: `server.serve` speaks plain HTTP only. Every deployment
of a witchy server therefore requires a TLS-terminating proxy in front of it —
including self-hosted coven registries, where "run your own registry" is part
of the pitch and "also install and configure a reverse proxy" undercuts it.
Serving HTTPS is table stakes for a server-shaped stdlib.

Doing it the witchy way also produces a property worth having: a TLS private
key is the most valuable secret a server process holds, and the existing
secrets model (guest holds an opaque handle; the host holds the bytes —
[`crates/witchy-runtime/src/runtime.rs:209-211`](../crates/witchy-runtime/src/runtime.rs), the signing-key pattern) means
witchy can perform the handshake **without the key ever entering guest
memory**. A bug in the server program — request smuggling, a deserialization
flaw, even guest-memory corruption — cannot exfiltrate a key the guest cannot
address.

One honesty gap must be closed for that claim: `crypto.reveal` can reveal a
granted value secret ([`runtime.rs:732`](../crates/witchy-runtime/src/runtime.rs)). A key that *can* be revealed is only
conventionally protected. Hence the use-only flag below.

## Design

### std surface

```sh
# std/server additions (signatures; not a runnable example)
pub fn serve_tls(net: Net[Listen, Tcp], addr: String,
                 cert_pem: String, key: Secret, app: Router)
pub fn serve_tls_n(net: Net[Listen, Tcp], addr: String,
                   cert_pem: String, key: Secret, app: Router, n: Int)
```

- `cert_pem` is the public certificate chain (PEM text — inline, or read via
  an ordinary `Dir` grant; it is not secret).
- `key` is a `Secret` obtained the normal way
  (`secretstore.require(store, "tls-key")`), so serving TLS requires the
  `SecretStore` grant in the capability footprint — visible, as authority
  should be.
- Handlers, `Router`, `Request`/`Response` are unchanged: TLS is transparent
  above the accepted connection.

### Runtime

- A new host import `net_listen_tls(net, addr, cert, key_handle)` builds a
  rustls `ServerConfig` once at listen time and returns a listener handle;
  accepts on it perform the handshake host-side and yield ordinary `Socket`
  handles — the existing read/write host fns are untouched (the host wraps the
  stream).
- **rustls**, no OpenSSL; pinned latest per dependency policy.
- **Both backends.** The interpreter's net implementation gains the same
  behavior; the rustls plumbing lives in one shared host module so the two
  backends cannot drift (parity by shared implementation, plus differential
  e2e below).
- `serve_pool` compatibility: workers already share one listener and accept
  concurrently; TLS state is per-connection, so each worker handshakes its own
  accepts. No changes to the pool model.

### Error policy (per RFC-0044)

- Malformed cert or key, or a key that does not match the cert: **loud error
  at listen time** — fail at startup, not at first connection.
- A failed handshake on an individual connection (plaintext client, bad
  ClientHello, unsupported version): drop the connection and continue
  accepting. Per-connection TLS failures are the network's weather, not
  program errors.

### Use-only secrets (the honesty mechanism)

Add one bit to a secret grant: `use-only`. A use-only secret can be passed to
host operations that consume it by handle (TLS serving, signing) but
`crypto.reveal` on it errors. Grant syntax follows the existing `--secret`
flag shape (e.g. `--secret tls-key=@key.pem,use-only`; exact spelling settled
at implementation with the flag's current grammar).

- Default remains revealable (no behavior change for existing programs).
- `serve_tls` works with either, but the deployment guidance and the coven
  templates grant TLS keys use-only.
- The signing key should adopt the same flag in coven's own deployment — the
  mechanism strengthens the existing pattern, it isn't TLS-specific.

### Testing

- e2e on **both backends**: `serve_tls` with a self-signed fixture, exercised
  by the RFC-0009 HTTPS client; wrong-key-for-cert fails loudly at startup;
  a plaintext request to the TLS port is dropped and the server keeps serving.
- The client must trust the test CA through the scoped, feature-gated test-root
  registry. Production builds do not expose that registry; their trust remains
  the Mozilla root set plus explicitly configured `WITCHY_TLS_EXTRA_ROOTS`.
- A use-only test: `crypto.reveal` on a use-only secret errors identically on
  both backends.

## Out of scope

ACME/automatic renewal (rotation = restart for now), mTLS/client certificates,
SNI multi-certificate serving, ALPN/h2 (the server is HTTP/1.1 today), and
cert hot-reload. Each is compatible with this design and none blocks it.

## Alternatives

- **Stay proxy-only**: acceptable for the hosted registry (which fronts with a
  CDN regardless) but wrong for the stdlib — it makes every self-hosted
  deployment two pieces of software, and leaves origin traffic plaintext even
  in proxied setups.
- **Key as a file path / plain String**: simplest, and how most stacks do it —
  but it hands the key bytes to guest memory, discarding the property the
  secrets model already paid for. Rejected.
- **TLS as part of the Net capability grant** (e.g. cert/key configured on
  `--net`): moves policy host-side entirely, but couples an application
  concern (which cert to serve) to the authority grant and would make one Net
  grant unable to serve two endpoints differently. The `Secret`-parameter
  design keeps authority (SecretStore) and configuration (which key) cleanly
  separated. Rejected.

## Drawbacks

- rustls (plus its dependency tree) joins the trusted computing base of every
  networked deployment. It is the strongest candidate available for that seat,
  and it is already present on the client side.
- The use-only flag is a small but real extension of the secrets model
  (grant syntax, one bit in the host secret table, an error path in reveal) —
  scoped here because the TLS claim is dishonest without it.
- Startup-time key loading means rotation requires a restart; fine at current
  scale, noted for the future.

## Prior art

- The in-repo signing-key pattern (`secret_seed_bytes`, host-side use by
  handle) — this RFC generalizes it to a second consumer and hardens it with
  use-only.
- [RFC-0009](0009-https-tls-client.md) (HTTPS client) — the counterpart surface and the shared rustls
  seat.
- Go's `http.ListenAndServeTLS` — the API shape callers expect (cert + key +
  serve); its keys live as plaintext bytes in process memory, which is the
  design this RFC deliberately departs from.
- HSMs / cloud KMSes — the same use-don't-read principle for key material,
  here provided by the host/guest boundary instead of hardware.
