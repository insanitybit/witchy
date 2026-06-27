---
rfc: 0009
title: HTTPS / TLS client — a tls: address scheme, terminated host-side
status: implemented
created: 2026-06-23
implemented: 2026-06-24
tracking: commit 8c791bb (TLS), c864408 (std/http HTTPS)
---

# RFC-0009: HTTPS / TLS client — a `tls:` address scheme, terminated host-side

> Code blocks here are intentionally **not** tagged `witchy` (per RFC-0002's
> convention) so the doc-examples test does not try to compile partial snippets.

> **Status: implemented** (2026-06-24). The shipped design diverges from the
> original proposal in two ways, both noted inline: the TLS crate is **rustls +
> aws-lc-rs** (not s2n-tls), and the `tls:` scheme is a **connect-time choice**
> stripped before allowlist matching (not an independent allowlist entry). See
> *Implementation notes*.

## Summary

Give witchy programs the ability to make outbound **HTTPS** (and any TLS) calls.
witchy networking was plaintext TCP only; this adds a host-terminated TLS client.
The key design choice: **TLS is not a new capability and not a new right.** It is a
property of the *endpoint* you connect to — a `tls:` scheme on the address you dial
(`tls:github.com:443`). `std/http` dials a `tls:` address when a URL's scheme is
`https://`. The TLS handshake, certificate verification, and record encryption
happen **on the host** (rustls configured with the **aws-lc-rs** crypto provider —
the same aws-lc backend witchy already uses for SHA-256 / Ed25519 / ECDSA / RSA);
the guest reads and writes plaintext over an opaque socket handle, exactly as it
does for plaintext TCP.

The capability model **does not change**. `Net` stays the one network primitive;
its existing right-typing (`Net[Connect, Tcp]`) is unchanged, because TLS rides on
TCP — at the transport layer an HTTPS call is still `Net[Connect, Tcp]`. The
language stays byte-free, the sandbox stays host-mediated, DNS stays where it
already lives (host-side, inside `connect`, governed by the same allowlist), and
both backends behave identically.

## Motivation

Every real external integration on coven's roadmap needs HTTPS, and none of it was
reachable before this:

- **OIDC trusted publishing** must fetch a provider's signing keys (JWKS) —
  `https://token.actions.githubusercontent.com/...`, `https://www.googleapis.com/oauth2/v3/certs`.
- **Social login** (`rfcs/0010-web-console-social-login.md`) must do the OAuth
  code→token exchange and userinfo call against `https://github.com` /
  `https://oauth2.googleapis.com` — server-side, because the client secret can
  never reach the browser and coven-web's `connect-src 'self'` forbids a
  cross-origin `fetch`.
- Any future "fetch an attestation / a remote manifest / a webhook" is HTTPS.

Before this RFC, `src/runtime.rs` dialed with `std::net::TcpStream` (plaintext) via
the `net_connect` host op, and `Cargo.toml` had no TLS crate. So the registry could
talk to itself over localhost, and to nothing else.

## Design

### The one rule: where does each concern live?

An earlier draft made TLS a new **right** on `Net` (`Net[Connect, Tls]`). That
conflates three different axes that already exist at three different layers, so this
design places each concern where the model already puts its peers:

| Concern | Belongs to | Where it lives |
|---|---|---|
| *May I dial out at all?* (the verb) | a **right** | `Net[Connect]` |
| *Which transport?* | a **right** | `Net[..., Tcp]` |
| *Which host? which port?* | the **endpoint** | the allowlist string, host-enforced |
| *Encrypted or not?* | the **endpoint** | the `tls:` scheme on the dialed address |
| *Which protocol does this request speak?* | **data** | the `Url`'s scheme |
| *Resolve a name to dial it* (DNS) | folded into `connect` | the allowlist (already) |

TLS belongs in the **endpoint** rows, next to host and port — not in the rights
rows. Whether the connection to an endpoint is encrypted is the same kind of fact as
which host and port it is, so it lives at the same layer.

### TLS is a `tls:` scheme on the dialed address (not a right)

A guest opens an HTTPS connection by dialing a `tls:`-schemed address; the host
performs the handshake and returns a plaintext socket:

```
connect(net, "github.com:443")        plaintext TCP            (today)
connect(net, "tls:github.com:443")    TLS over TCP (HTTPS)     (this RFC)
```

`Net`'s type is unchanged — `caps` still shows `Net[Connect, Tcp]` — because at the
transport layer HTTPS *is* TCP.

**Implementation note — the scheme is a connect-time choice, NOT an allowlist
entry.** `src/net.rs::parse_scheme` strips a leading `tls:` from the *dialed
address* before the allowlist check, so the **allowlist governs the bare
`host:port`** (scheme-agnostic). A program granted `--net github.com:443` may dial
either `github.com:443` (plain) or `tls:github.com:443` (TLS); the `tls:` is its
choice at the call site, and the host validates the endpoint either way. This is
**simpler than the original proposal**, which made `tls:` and plaintext *independent
allowlist entries* to provide a hard no-downgrade guarantee at the authority layer.
That guarantee was traded for simplicity: TLS is strictly *safer* than plaintext, so
permitting an endpoint and electing TLS at the call site is sound for the use cases
here (a plaintext dial to an HTTPS-only server like GitHub simply fails). A future
revision could restore scheme-pinned allowlist entries if a hard, authority-layer
"this `Net` can ONLY speak TLS" guarantee is ever required — that would be a
`net_allows` change in `src/capabilities.rs`, not a language change.

So the allowlist/`restrict`/`--net` forms stay **scheme-free**:

```
--net github.com:443                       # grants github.com:443 (plain OR tls)
let gh = restrict(net, "github.com:443")   # narrows to that host:port
connect(gh, "tls:github.com:443")          # elects TLS at the call site
```

### The host op (both backends, parity-preserving)

No new capability surface in the type system. The existing `connect` / `try_connect`
host ops (`src/runtime.rs` for WASM, the `connect` builtin in `src/interpreter.rs`)
route through a shared dialer, `src/net.rs::dial`:

```
connect(net, addr)      -> socket | error      # addr may be `tls:host:port`
try_connect(net, addr)  -> socket | None
```

When `addr` is `tls:`-schemed, `dial`:

1. Strips `tls:` (`parse_scheme`), then resolves + allowlist-checks the bare
   `host:port` (`resolve_admitted` in `src/capabilities.rs`, identical to plaintext
   — name resolution pinned to the resolved IP, rebinding-safe).
2. Opens the TCP socket, then completes the TLS handshake with **rustls** configured
   to use the **aws-lc-rs `CryptoProvider`** — so all crypto stays on aws-lc (FIPS),
   never ring. **SNI** is the host taken from the address.
3. **Verifies the server certificate** against the trust roots (chain, hostname,
   validity). A verification failure is a hard error, never a downgrade.
4. Wraps the stream in `rustls::StreamOwned` and stores it in the same socket table
   as a plaintext `TcpStream`, behind a `Stream` trait object (`net.rs::Stream`:
   `Read + Write + Send` with a `shutdown` that drives close-notify for TLS). The
   guest's subsequent reads/writes are **plaintext**; TLS is transparent.

Because the socket table is host-side and both backends route `connect`/`send`/`recv`
through it, **the compiled (WASM) path gets TLS for free through the same host
import — no WIR/codegen change, so parity holds**. TLS never runs inside the guest,
so aws-lc's lack of a wasm32 build is irrelevant. The in-browser RFC-0007 target is
pure-compute (no `Net`) and never reaches this op.

### `std/http` over HTTPS — URLs, not `Http`/`Https` types

The scheme is a property of the *resource*, so it lives on the `Url` (`std/url`
already parses it, defaulting the port to 443 for `https`), not in a new capability
type. `std/http` keeps taking a plain `Net[Connect, Tcp]` and dispatches on the URL
scheme: `https://` builds a `tls:host:port` address, `http://` a plaintext one.

```
# https:// detected from the URL; port defaults to 443; a tls: address is dialed.
# Both require only Net[Connect, Tcp]. Response parsing/headers/bodies unchanged.
pub fn get_url(net: Net[Connect, Tcp], raw: String) -> Result(Response, String)
```

`request_with`/`try_request_with` gained a `secure: Bool` selecting the `tls:` dial;
the URL-based entry points (`get_url`, the `RequestBuilder.send`) set it from the
parsed scheme. There are **no `Http`/`Https` capability types** in the common path:
one `Net` plus a `Url` covers both protocols.

### The client shape — the existing fluent builder, now over HTTPS

`std/http` already shipped a `reqwest`-style request builder. HTTPS adds **no new
method and no new type**; the builder takes a full URL and `.send()` dials a `tls:`
address whenever the URL's scheme is `https://`. The only witchy-ism is that the
network authority is an explicit argument to `.send(net)`, never an ambient client:

```
let r = http.get_request(url)
    .with_header("authorization", "Bearer ${tok}")
    .with_header("accept", "application/json")
    .send(net)?                         # net: Net[Connect, Tcp]
let body = http.body(r)
```

This is the surface the social-login flows use (RFC-0010): `http.post_request(
"https://github.com/login/oauth/access_token").with_body(form).send(net)` for the
code→token exchange, and a bearer `GET https://api.github.com/user` for the user —
both real, both tested end to end.

### Trust roots — as implemented

Certificate verification is **on by default and mandatory**. The root store is:

1. The **Mozilla CA set** via the `webpki-roots` crate (a vendored, reproducible
   trust anchor set — chosen over the OS store for deterministic behavior across
   hosts and CI).
2. Plus any **extra PEM roots** named by the `WITCHY_TLS_EXTRA_ROOTS` environment
   variable (a path to a PEM file), parsed with `rustls-pemfile`. This covers a
   custom/corporate CA and is how the hermetic tests trust a local mock IdP's
   self-signed cert.

There is **no ambient "skip verification"** — a self-signed endpoint must have its
cert added via `WITCHY_TLS_EXTRA_ROOTS`, which still validates the chain against that
explicit root. (The earlier draft's `--ca-file` / `--tls-insecure` flags were not
built; the env-var root hook subsumes the custom-CA need without a "skip" escape.)

### Errors, never downgrade

Handshake failure and certificate-verification failure are `Result::Err` with a
specific reason (or a trap on the non-`try_` path). There is **no automatic
fall-back to plaintext**: an `https://` request that cannot establish a verified TLS
session fails loudly. (For OIDC this is essential — a silent downgrade is an identity
bypass.)

## Implementation notes (divergences from the original proposal)

1. **Crate: rustls + aws-lc-rs, not s2n-tls.** s2n-tls's Rust bindings are a
   *polling* model (`poll_negotiate`/`poll_recv` + C send/receive callbacks) with no
   safe blocking stream, so a blocking TLS client on witchy's blocking socket layer
   would have needed unsafe C-callback glue on a security-critical path. rustls with
   the `aws_lc_rs` provider gives a safe blocking `StreamOwned` and keeps **all
   crypto on aws-lc** (the original objection to rustls — a second crypto stack via
   `ring` — does not apply when the provider is aws-lc-rs). Cargo: `rustls = {
   default-features = false, features = ["std", "aws_lc_rs", "tls12", "logging"] }`,
   `webpki-roots`, `rustls-pemfile`.
2. **`tls:` is a connect-time scheme, not an independent allowlist entry** (see the
   scheme section above) — the allowlist is scheme-agnostic `host:port`; the hard
   authority-layer no-downgrade guarantee was not built.

## Alternatives

- **A `Tls` right on `Net` (`Net[Connect, Tls]`)** — an earlier draft. Rejected:
  conflates a session-layer property (encryption) with the verb/transport rights, and
  is a bigger change (type system, footprint printing, grant parser, both backends)
  for no gain the endpoint scheme does not already provide.
- **`Http` / `Https` as capability types** — splits `std/http`'s API in two (or needs
  generics witchy lacks) and encodes in the type a fact that is really request data.
  Rejected in favor of URLs-as-data.
- **A separate `Dns` capability/right** — rejected: DNS is already an implicit,
  confined sub-step of `connect` governed by the allowlist.
- **s2n-tls over aws-lc** — the original choice, to keep the whole TLS+crypto stack
  in the AWS/FIPS ecosystem. Rejected at implementation time for the blocking-I/O
  ergonomics above; rustls+aws-lc-rs keeps the crypto on aws-lc anyway.
- **In-language TLS** — impossible (no byte access), the reason crypto lives at the
  native seam. Rejected.
- **`native-tls` / platform TLS** — platform-divergent trust handling; loses the
  single FIPS-capable backend. Rejected.
- **A local TLS-terminating proxy** — a non-witchy moving part the operator must run,
  against the self-hosting/dogfood goal. Kept only as a documented stopgap.

## Drawbacks

- **A C build in the toolchain.** aws-lc-rs builds via cmake (already a dependency);
  rustls is pure Rust. No wasm32 TLS build — acceptable, since TLS is host-side only.
- **TCB growth.** A TLS stack and certificate-path validation enter the trusted base.
  Irreducible for HTTPS; rustls + aws-lc-rs is a widely-audited, FIPS-capable choice.
- **Trust-root maintenance.** `webpki-roots` is a pinned set that must be kept current
  via dependency updates; a stale set fails closed (good) but can break calls.

## Prior art

- `aws-lc-rs` (already a dependency) — the FIPS-capable crypto, here as rustls's
  provider.
- **rustls** — the protocol layer; `StreamOwned` is the safe blocking stream that
  drops into witchy's host socket table.
- **webpki-roots** / **rustls-pemfile** — the Mozilla trust anchors and the extra-root
  PEM reader behind `WITCHY_TLS_EXTRA_ROOTS`.
- RFC-0003 (network address scoping) — the `Net` allowlist + host:port matcher this
  extends.
- `rfcs/0010-web-console-social-login.md` and the coven trusted-publishing flow — the
  two consumers that made this a prerequisite, both now shipped on top of it.
