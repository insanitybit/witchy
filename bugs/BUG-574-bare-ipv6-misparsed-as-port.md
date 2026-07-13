# BUG-574: Bare IPv6 authorities were misparsed as host and port

- **Severity:** MED
- **Status:** FIXED
- **Component:** `std/url`, authority parsing
- **Found:** 2026-07-12

## Summary

`url.parse` documented unbracketed IPv6 literals such as `::1` as host-only
authorities using the scheme's default port. `host_port_split` nevertheless
split every unbracketed authority at its last colon. Consequently:

- `http://::1` became host `:` with port `1`;
- `http://2001:db8::1/path` became host `2001:db8:` with port `1`.

Formatting reconstructed similar text, so round-trip-only coverage did not
detect the corrupted host and port fields.

## Resolution

An unbracketed colon is a port separator only when it is the authority's sole
colon. Multiple colons identify the existing bare-IPv6 form and keep the whole
authority as the host with the scheme's default port. IPv6 with an explicit
port remains bracketed and unchanged: `[::1]:8080`.

`tests/typed_errors.rs::url_typed_errors_are_matchable_and_bridge_to_string`
asserts the host and port fields for short and full bare IPv6 literals. The
existing bracketed-IPv6 coverage continues to pin explicit-port behavior.
