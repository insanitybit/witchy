# BUG-047: `witchy verify` does not match the documented offline provenance contract

Severity: HIGH
Status: FIXED
Verified: 2026-07-12 TESTED on `fix/rfc0054-final-trust-boundaries`
Component: Package manager verification, registry docs
Found: 2026-07-04

## Summary

The package-manager docs tell users that `witchy verify` re-checks lock hashes,
signatures, and provenance offline. The implemented `verify` command does not
provide that contract for registry dependencies.

Current `verify` does two useful things:

- It recomputes hashes only for path dependencies listed in `witchy.toml`.
- If the lock pins registry metadata, it contacts the registry and verifies the
  TUF snapshot/timestamp chain plus each vendored `coven.json` record digest
  against the signed snapshot.

The offline vendored-source security check lives in separate commands:
`verify-rune <dir> <rootpub>` and `verify-vendor <dir> <rootpub>`. Those commands
verify each vendored rune's `coven.json` signature and recompute the on-disk
source hash against the signed record.

That leaves the release-facing command contract split in a surprising way:
`witchy verify` can be green without performing the offline registry-source
verification that the docs say it performs.

## Evidence

- `book/src/packages.md:91-94` says registry metadata is signed and lockfiles
  pin content hashes, the registry key, and the full provenance chain, "all
  re-checkable offline with `witchy verify`."
- `book/src/packages-cli.md:92` describes `witchy verify` as "re-check the
  lock's hashes/signatures/provenance offline."
- `spec/local-registry.md:106-108` says `witchy verify` re-checks the content
  hash, signing key fingerprint, and trusted-publishing provenance chain
  offline.
- `rfcs/package-manager.md:588` says `witchy verify` re-verifies the lock
  against the store and coven signatures.
- `projects/pm/src/pm.witchy:923-934` implements `cmd_verify` as
  `report_verify(hash_mismatches(...))` plus `verify_tuf(...)`.
- `projects/pm/src/pm.witchy:1111-1119` shows `hash_mismatches` only iterates
  `witchy.toml` path dependencies and recomputes hashes for entries with
  `path = ...`; it does not recompute hashes for vendored registry dependencies
  listed as `source = "coven"` in `witchy.lock`.
- `projects/pm/src/pm.witchy:958-1032` verifies the online TUF chain and checks
  vendored `coven.json` payload digests against snapshot targets, but does not
  run `record_signature_valid` on each vendored record or compare each vendored
  source tree hash to the record hash.
- `projects/pm/src/pm.witchy:1848-1907` implements that offline signed-record
  plus source-content check in `verify-rune` and `verify-vendor`, which require
  a separate root public key argument.
- `projects/pm/src/pm.witchy:2075-2089` help exposes these as separate commands:
  `verify <dir>` is only described as "witchy.lock must match the dependency
  sources", while `verify-rune` and `verify-vendor` are the offline vendored
  verification commands.

## Why this is a release gap

This is a security-contract mismatch, not just cosmetic docs drift. The public
story says users can run one verification command over a lockfile and get
offline assurance about content, signatures, and provenance. The actual command
requires users to know that registry provenance is verified by a different
subcommand with an explicit trust-anchor argument.

It is distinct from BUG-035, which tracks broad package CLI/docs mismatch. This
bug is the specific trust-boundary contract of the verification command.

## Expected fix

Pick one public verification contract and make it mechanically true:

- Preferred: make `witchy verify` the documented release gate. It should read the
  pinned registry root key from `witchy.lock`, verify vendored `coven.json`
  signatures/provenance, recompute every vendored registry source hash, and make
  online TUF freshness checking an explicit optional/online phase if needed.
- Or revise the public docs to distinguish the commands precisely:
  `verify` for path-lock/TUF metadata checks and `verify-vendor <dir> <rootpub>`
  for offline registry-source provenance.

## Acceptance

- A tampered file under `vendor/<name>/src/` makes the documented verification
  command fail for a registry dependency.
- A tampered `vendor/<name>/coven.json` signature or provenance field makes the
  documented verification command fail.
- The book, spec, RFC status table, and `witchy pm` help all name the same
  command for the same verification guarantees.
- Tests cover a healthy vendored registry dependency and both tampered-source and
  tampered-record cases through the documented command.

## Resolution

`verify`, `build`, and `run` now share one typed offline lock-integrity gate. It
checks path hashes, requires every locked registry vendor, verifies each adjacent
record with the root key pinned in the lock, binds signed name/version/hash to
the lock coordinate, and recomputes the vendored source hash. Live TUF freshness
and rollback checks are the explicit additive `verify --online` phase.

The e2e regression runs ordinary `verify` against an unreachable registry and
then proves source tampering, signature tampering, coordinate substitution,
missing root pins, source-kind substitution, and missing vendors all fail.
