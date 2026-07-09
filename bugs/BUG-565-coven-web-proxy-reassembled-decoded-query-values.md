# BUG-565: Coven-web proxy reassembled decoded query values without encoding

Status: FIXED
Severity: MEDIUM
Component: `projects/coven-web`, HTTP query forwarding

## Summary

`std/server` percent-decodes request query keys and values before storing them
in `Request`. Coven-web's read-only reverse proxy then joined those decoded
strings directly with `=` and `&`.

As a result, a request such as:

```text
/api/coven/versions?name=acme%26state%3Dyanked
```

was forwarded as two upstream parameters:

```text
/coven/versions?name=acme&state=yanked
```

The proxy route allowlist limits this to read endpoints, but changing parameter
structure across a trust boundary is still incorrect and could alter filtering
or selection semantics in present or future handlers.

## Resolution

`encode_query` now applies `url.encode` to every decoded key and value before
assembling the upstream query string. The Coven-web e2e captures the mock
upstream's request line and verifies encoded separators remain value data.
