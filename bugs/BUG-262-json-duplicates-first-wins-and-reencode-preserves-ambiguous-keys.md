# BUG-262: JSON duplicate object names can still be constructed and re-encoded

Severity: MED
Status: FIXED
Verified: 2026-07-09 SOURCE+TEST on branch fix/bug262-json-accessor-duplicates
Area: `std/json`, JWT/OIDC claim handling, Coven request parsing, package record verification

## Current status

Current `json.decode` rejects duplicate object names while parsing, so the old
default-decoder ambiguity for incoming wire JSON is fixed in the core parser.
The release-facing emission/canonicalization boundaries also fail loudly when
handed duplicate object names: `json.encode`, `json.encode_pretty`,
`json.object_sorted`, and `json.merge`.

The remaining first-wins read path is now fixed too: `json.get`, typed accessors
that compose through it (`get_string`, `get_int`, etc.), `json.contains_key`,
and `json.as_object` reject duplicate object names before exposing pairs. A
hand-built ambiguous `JsonObject` can still exist as a raw ADT value, but no
stdlib JSON accessor, encoder, merge, sorted-object builder, reflection path, or
default decoder silently chooses an interpretation for it. Sealing or hiding
the raw constructors would be a larger API design change, not the remaining
BUG-262 runtime ambiguity.

Regression for the fixed boundaries:

```sh
CARGO_TARGET_DIR=target-codex-json cargo test json_duplicate_object_keys_fail_at_encoding_boundaries_on_both_backends -- --nocapture
```

## Historical problem

`std/json` represents objects as `List((String, Json))`, accepts duplicate
object names while decoding, and preserves every pair while encoding. The
public accessors (`json.get`, `get_string`, `get_int`, etc.) return the first
matching pair.

That gives Witchy a first-wins interpretation for application logic, while the
wire JSON emitted or accepted by the same library can still contain ambiguous
duplicate names. RFC 8259 section 4 says object member names SHOULD be unique
and explicitly calls duplicate-name behavior unpredictable across receivers:
some report the last pair, some error, and some report all pairs.

In release-facing code, this is not just cosmetic. JWT/OIDC claim checks, JWKS
selection, Coven JSON helpers, and package-manager signed record verification
all consume fields through `json.get*`/`jget`, so a duplicate-key payload can
make Witchy authorize, display, or sign one interpretation while another JSON
consumer sees a different one.

## Evidence

- `std/json.witchy:19-26` exposes `JsonObject(List((String, Json)))`, so callers
  can construct duplicate-key objects directly.
- `std/json.witchy` now rejects duplicate keys before compact/pretty encoding,
  deterministic `object_sorted`, and shallow object merge boundaries.
- `std/json.witchy:514-516` now rejects duplicate keys during `json.decode`,
  using `pairs_contains_key` before pushing each parsed member.
- `std/json.witchy` rejects duplicate keys in `json.get`,
  `json.contains_key`, and `json.as_object`, so typed accessors and callers
  such as JWT/Coven/PM fail closed instead of reading first-wins from a
  hand-built duplicate object.
- `std/json.witchy` says `merge(a, b)` gives `b` override semantics; duplicate
  names inside either input now fail instead of surviving into the merged object.
- `std/json.witchy` documents `object_sorted` as deterministic for signing;
  duplicate names now fail before a supposedly canonical object is built.
- `std/json.witchy` reflects a `JsonObject` back to fields by preserving every
  pair only after the same uniqueness check.
- `std/jwt.witchy:39-49`, `std/jwt.witchy:58-67`, and
  `std/jwt.witchy:146-160` validate `exp`, `aud`, `iss`, `nbf`, and JWK fields
  through the first-wins JSON accessors.
- `projects/coven/src/coven_json.witchy:9-23` wraps `json.get_string` and
  `json.get_int` for untrusted request bodies and identity-token claims.
- `projects/pm/src/pm.witchy:1930-1947` verifies signed registry records by
  deriving the canonical payload with `jget`/`json.get_strings`.

Reference: https://www.rfc-editor.org/rfc/rfc8259#section-4

## Why this matters for consistency

Witchy currently tells users that JSON parsing, JWT verification, registry
records, and deterministic signing are part of the coherent core platform. A
first-wins accessor model paired with a duplicate-preserving encoder is a hidden
policy choice, and it leaks through exactly the libraries users would rely on
for security-sensitive data.

This also creates an awkward API story:

- `dict.from_pairs` documents a last-pair-wins map-like rule.
- `json.get` is first-pair-wins.
- `json.merge` advertises override semantics but preserves duplicates already
  present in either input.
- `json.object_sorted` sounds canonical/signing-friendly while keeping repeated
  names.

## Suggested fix

Pick one object-name policy and make it impossible to miss:

1. Keep the fixed default decoder behavior: duplicate object names are rejected
   at every object level with a precise error.
2. Add a checked object constructor, or make `JsonObject` construction go
   through helpers if the language supports hiding constructors later.
3. Update `object_sorted` and `merge` to reject or normalize duplicates before
   producing objects described as deterministic/canonical.
4. If permissive duplicate-preserving parsing is still useful, expose it as an
   explicit alternate API such as `decode_pairs`/`decode_lossless`, not the
   default `json.decode`.
5. Add parity tests covering:
   - `{"aud":"good","aud":"evil"}` rejected by `json.decode`.
   - nested duplicate keys rejected by `json.decode`.
   - duplicate keys created through helpers cannot be re-encoded silently.
   - JWT/Coven/PM callers fail closed on duplicate fields.

## Release note

Treat this as a release-hardening bug before presenting `std/json`, JWT/OIDC,
Coven, or package records as security-ready. It is adjacent to BUG-245's TOML
duplicate-key issue, but affects JSON and signed/authenticated paths.
