# Coven TUF metadata in Witchy

This package implements the metadata framing and hash links for the Coven
TUF profile. It parses bounded UTF-8 JSON, checks the role and version, and
rejects duplicate decoded object names before exposing the document. For
signatures it encodes the complete `signed` object in the canonical JSON
dialect used by python-tuf. Object names are sorted by Unicode scalar order
and extension fields are included in the signed bytes. The dialect permits
integers and escapes quote and backslash in strings.
[TUF document formats](https://theupdateframework.github.io/specification/latest/#document-formats)
· [Reference canonical encoder](https://github.com/secure-systems-lab/securesystemslib/blob/main/securesystemslib/formats.py)

`MetadataReference.open` checks the referenced file's length and SHA-384 over
its original bytes before parsing it, then checks its role and version.
`ContentReference.verify` performs the same byte check for a bundle.
The caller must obtain a reference from independently authenticated
metadata. A parsed `Metadata` contains bytes for signature verification.

The verifier supports Coven's `ml-dsa-87/1` profile. Witchy checks the PEM
public key and raw signature encodings, builds the TAP 21 signature message
with SHA-512, and counts valid signatures from distinct authorized raw keys.
The existing AWS-LC provider performs SHA-512 and pure ML-DSA-87 verification.
[TAP 21](https://github.com/theupdateframework/taps/blob/master/tap21.md)

`bootstrap_root` checks an externally provisioned SHA-384 pin, root version 1,
the root's signature threshold, and distinct keys for the four top-level
roles. Consistent snapshots and at least two root signatures are required.
`TrustedRoot.advance` accepts only the next version and verifies both the
previous and replacement root thresholds. `TrustedRoot.verify` checks a
top-level role against that root. Delegated verification takes the role and
keys from its authenticated parent Targets document.

`verify_top_level` verifies the timestamp–snapshot–targets chain using the
original bytes and one update start time. All four top-level roles must
have valid UTC expiry dates and be current at that instant. With a protected
baseline, the verifier refuses a lower timestamp, snapshot, or targets
version, missing snapshot entries, and changes to the content at an existing
version. It accepts an exact repeat and keeps expiry-independent rollback
floors from previously published metadata.
[TUF update workflow](https://theupdateframework.github.io/specification/latest/#detailed-client-workflow)

The verified top-level object also checks that the snapshot lists exactly the
declared package roles. Each role has a disjoint set of `NAME/*` namespaces and
its own keys. `open_role` checks the child's signed reference, threshold,
expiry, and listed coordinates against those namespaces. Child roles cannot
delegate further. An empty role keeps its snapshot entry and withdraws all
its targets. `VerifiedRole.verify_target` checks the original bundle length
and SHA-384 for a listed coordinate.
[TUF delegated targets](https://theupdateframework.github.io/specification/latest/#file-formats-targetsjson-and-delegated-target-roles)

`VerifiedRole.verify_attestations` checks the same bundle bytes against
the TUF reference and the release authorities. It checks a separately
pinned release trust document, the publisher and witness signatures,
the selected credential's package scope, and the witnessed approval
window. Both signatures cover the exact release coordinate and package
digest. The witness signature also covers the approval receipt and
publisher signature. [Coven release approval](../approval.md)

The same check verifies the enrolled ES256 WebAuthn credential over the
original authenticator and client data. It requires the release challenge,
exact same-origin use, the relying-party hostname hash, and both user presence
and verification. The verifier uses the ES256/P-256 profile with no
authenticator extensions. The staging service owns the online counter and
challenge state. [WebAuthn assertion verification](https://www.w3.org/TR/webauthn-3/#sctn-verifying-assertion)

The release verifier checks the complete package record before using its
digest. For multi-file packages it checks canonical paths and dependencies,
the file table and every file's SHA-384, and the import closure from the entry.
Single-file packages retain their opaque body and canonical header.
[Package format](../README.md#coven-for-witchy-2)

The preparation program accepts a read grant over a tree containing
`incoming/`, `published/`, `initial-root.json`, and `release-trust.json`.
Its input console receives the externally provisioned root and release
trust SHA-384 pins as 48 raw bytes each, in that order. The caller holds
the publication lock and grants a fresh, private, nonoverlapping spool
as the only writable directory. The program compares incoming immutable
files with their published counterparts, loads the protected rollback
baseline, and rejects key reuse between TUF roles and release authorities.
It stages the verified bytes under their final relative paths and writes
the spool's timestamp only after every other file passes. The caller must
discard the spool after a refusal or interrupted run.

For target fetch and catalog generation, the selector returns the snapshot
version from the authenticated Witchy metadata together with its exact digest.
The launcher checks the bounded reply and checks that the raw root, timestamp,
and snapshot still match those digests before saving the rollback checkpoint.
When supplying a prebuilt selector with `--selector-module`, deploy it and
its independently verified pin from the same release as the launcher.

Run `grimoire test witchy2/tuf` from the Coven checkout. The
`coven tuf publish` launcher holds the operator lock from the rollback
read through the timestamp replacement. It gives Witchy the already-open
incoming and published directories as read-only grants, and promotes only
the private spool. It syncs each new file and its containing directory
before replacing the timestamp.
