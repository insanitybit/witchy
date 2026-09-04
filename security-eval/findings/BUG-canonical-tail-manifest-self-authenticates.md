# Canonical tail-site completeness is self-authenticated by lowering

## Status

Resolved on `impl/rfc0165-canonical-v1`. Production canonical cutover still
requires the RFC-0090 deep-transition acceptance probes.

## Impact

`CanonicalCheckedSource` now binds the exact final typed module to its
frontend-derived proper-tail manifest. Canonical lowering copies those sites
into `CanonicalModule::proper_tail_sites`. The initial SafeWIR checker verifies
that each site in that candidate list is represented exactly once by a
`TailCall` terminator.

The checker does not receive an independent manifest witness or its digest.
A lowering bug that omits a proper-tail source site can therefore omit the
same site from the candidate list. The candidate then passes completeness
checking even though an ordinary `Call; Return` can violate Witchy's constant
stack guarantee.

## Required fix

Bind the frontend-derived proper-tail manifest digest into the opaque checked
source product and carry it unchanged as an independent lowering input. The
SafeWIR checker must recompute the digest of the candidate's tail obligations
and reject any mismatch before minting initial authority. Optimization history
must preserve that independently bound digest through every revision.

The expected digest must not be recomputed by the lowerer from the candidate
list. Missing, duplicated, unknown, or ordinary-call encodings require focused
negative tests, followed by the RFC-0090 300,000-transition probes at final
cutover.

## Resolution

`witchy-types::pipeline::CheckedSourceWitness` is now privately minted by the
source-bearing type-check pipeline and binds both the exact source-bundle digest
and the frontend-derived proper-tail manifest digest. `CanonicalCheckedSource`
privately pairs that witness with the typed module and manifest. Initial SafeWIR
validation compares the lowered candidate's independently recomputed tail
digest with this frontend witness before minting lowering authority; final
validation and optimization history preserve the same digest.

Focused evidence:

```text
cargo test -p witchy-wir --features native canonical_safewir::authority::tests::initial_checker_rejects_an_independent_tail_manifest_mismatch -- --exact
cargo test -p witchy-lower canonical_lower::tests::proper_tail_source_lowers_with_manifest_identity -- --exact
```
