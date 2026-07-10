# BUG-572: PM registry trust pins were not one atomic invariant

Status: FIXED
Severity: HIGH
Component: `projects/pm`, TUF/TOFU lock metadata, RFC-0054

## Summary

`registry_snapshot_version` had a typed parser, but `registry_rootpub` was read
independently as a raw string and defaulted to `""`. An existing registry lock
with a missing or malformed root key therefore behaved like an old unpinned
lock: verify, fetch, and update could silently fall back to trust-on-first-use.
The inverse half-state (a root key without a snapshot floor) was also accepted.

Lock generation fetched the root key twice: once to verify the snapshot and
again to serialize `registry_rootpub`. A registry or mirror could return key A
for verification and key B for serialization, leaving a freshly generated lock
whose trust anchor did not authenticate its pinned snapshot.

## Resolution

`pinned_registry_trust` parses the snapshot version and 32-byte Ed25519 root key
as one `Result(Option((Int, String)), LockPinError)`: path-only projects omit
both fields; registry locks require both fields, valid together. Every PM
trust consumer uses that boundary, so corrupt or half-present state blocks with
a typed diagnostic instead of becoming unpinned trust.

Root-key network fetches now share one typed, format-validating boundary.
Lock generation fetches the key once and serializes the same value that verified
the snapshot. End-to-end coverage rejects malformed, missing, and orphaned lock
pins before network verification.
