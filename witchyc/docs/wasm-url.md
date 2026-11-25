# HTTP origins and URLs

`import url` parses HTTP identifiers from a `Span`. The returned values
borrow their input and keep the original bytes. Parsing requires no
allocator, connection, clock, or other capability.

- `url.parse_authority` reads an ASCII hostname and optional decimal
  port. `hostname()` gives the host used for DNS or TLS server names.
  `explicit_port()` gives zero when the input omits the port.
- `url.parse_origin` reads the scheme and authority. `authority()` gives
  the original bytes for an HTTP Host field, `hostname()` gives the DNS
  spelling, and `port()` supplies 80 or 443 when omitted. `is_https()`
  identifies the scheme. The canonical spelling omits the default port.
- `url.parse_url` accepts up to 8192 bytes. `origin()` gives the typed
  origin and `resource()` gives the original path and query, including
  the question mark. For an HTTP origin-form request target, supply a
  leading slash when the resource is empty or begins with a question mark.

The profile accepts lowercase ASCII host labels up to 63 bytes each and
253 bytes in total. Applications supply pre-encoded IDNA labels when
needed. It rejects user information, bracketed IPv6 literals, fragments,
whitespace, non-ASCII resource bytes, malformed percent escapes, and
ports with leading zeroes. It preserves percent encoding and path
segments. Callers still apply route admission and origin trust rules,
choose a matching `Net` grant, and authenticate the TLS peer.

Coven uses these views for TUF and CI HTTPS requests, mirror
authorities, and WebAuthn approval origins. WebAuthn separately admits
cleartext only for the exact localhost development host. The TUF
launcher allows an explicit IPv4 loopback test port and uses one
canonical HTTPS spelling for its rollback state.

[HTTP origin and normalization](https://www.rfc-editor.org/rfc/rfc9110.html#section-4.2.3) ·
[URI components and percent encoding](https://www.rfc-editor.org/rfc/rfc3986.html#section-3)
