# Canonical tail-site completeness is self-authenticated by lowering

## Status

Open. Production canonical cutover must not claim RFC-0090 completeness until
this is closed.

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
