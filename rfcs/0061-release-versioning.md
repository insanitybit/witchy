---
rfc: 0061
title: Release versioning — 0.x policy and the 0.1.0 gate
status: implemented
created: 2026-07-04
tracking: the versioning policy, tagged-build identity, and release gate machinery are implemented; cutting 0.1.0 remains an operational event governed by RELEASE-READINESS.md
---

# RFC-0061: Release versioning — 0.x policy and the 0.1.0 gate

The version invariant is checked by [`scripts/release-version.sh`](../scripts/release-version.sh)
and the private release workflow in [`.github/workflows/release.yml`](../.github/workflows/release.yml).

## Summary

witchy is currently unversioned in practice: `Cargo.toml` says `0.1.0` but
there are zero tags, no changelog, and no stated meaning for a version number.
This RFC defines the 0.x policy (what a release promises, which is
deliberately little), the release mechanics (tags, changelog, the existing
release workflow), and the gate for calling something **0.1.0** — the first
number the project will say out loud.

## Motivation

The repo is heading to public. "What version is this?" is the first question a
public project gets, and "unversioned" reads as "unfinished thinking" even
when the code is strong. Conversely, committing to too much (stability
guarantees, deprecation windows) contradicts the deliberate pre-1.0 policy of
one-cut breaking migrations. The version scheme must say exactly what is true:
this is early, it moves fast, releases are real checkpoints.

## Design

### 0.x semantics

- **0.x releases may break anything**: language surface, stdlib, tooling, the
  coven registry protocol. Breaking changes are one-cut migrations (no
  aliases, no deprecation shims — the existing policy, now stated publicly).
- What a release DOES promise: it is a tagged, reproducible snapshot; CI was
  green (`./scripts/check.sh --full` equivalent); the book, spec, and examples
  in that tag describe that tag (the executed-docs harness enforces this).
- Version format `0.MINOR.PATCH`: MINOR for feature/breaking releases, PATCH
  for fix-only releases. No date-based versions.

### Mechanics

- Annotated git tags `v0.MINOR.PATCH`; the existing tag-triggered release
  workflow builds and publishes binaries (already implemented; unexercised).
- `CHANGELOG.md` at the repo root: curated, per-release, written for users
  (what changed and what breaks), not generated from commit logs — the git
  history is planned to be purged before going public, so the changelog is
  authored fresh at 0.1.0 as a description of what 0.1.0 *is*, and appended
  per release thereafter.
- The `witchy` binary reports the version (`--version`) sourced from
  `CARGO_PKG_VERSION`; release binaries additionally embed the tag's commit.
  The tag workflow derives that commit from the checked-out `HEAD`, injects it
  only into the packaged release build, and fails before packaging unless the
  binary contains that exact commit. Native matrix builds additionally execute
  `--version` and require the exact tag-version/commit pair.
- Rune/registry versioning (packages already carry semver in `witchy.toml`)
  is unchanged by this RFC; this covers the language/toolchain release only.

### The 0.1.0 gate

0.1.0 is tagged when, and only when:

1. The open-RFC set of the 2026-07 review is drained — implemented, or
   formally deferred/rejected with the status recorded (per the review notes
   of 2026-07-04). Notably: the consistency cuts (0042/0043/0044+0049/0046),
   0053, 0058.
2. The prime-directive bug ledger is empty: no known behavioral divergence
   between the backends, and `bugs/` holds no OPEN entry above LOW severity.
3. Public-launch hygiene is done: history purged and re-initialized, docs
   surfaces deployed, release workflow exercised end-to-end once.

This makes "0.1.0" a checkable statement, not a mood: the version exists the
day the language stops needing apologies, which is the public-launch bar.


## Release checklist (operational — the tracked steps to cut a tag)

Run in order; each is checkable, nothing is a mood:

1. **Gate green**: `./scripts/check.sh --full` passes (build, clippy -D warnings,
   `witchy fmt` over std+examples, `nextest --workspace`, heap-check fuzz, wasm
   build, parity sweep, e2e, docs).
2. **RFC ledger drained**: no RFC in `rfcs/` sits in a non-terminal status
   (`proposed`/`planned`) that blocks the release; each open item is
   `implemented`, `accepted` (with tracking), `deferred`, `rejected`, or
   `superseded`.
3. **Bug ledger clean above LOW**: `bugs/` holds no OPEN entry above LOW
   severity; every prime-directive (backend-divergence/security) bug is closed.
4. **CHANGELOG written** for the tag, at release time, from the RFC + bug
   ledgers (a description of what 0.1.0 *is*, not a diff against history).
5. **Version stamped**: `Cargo.toml` version matches the tag; `witchy --version`
   reports it.
6. **Release workflow exercised** once end-to-end: `.github/workflows/release.yml`
   builds the artifacts, attaches `SHA256SUMS`, and the artifacts run.
7. **Tag** `v0.MINOR.PATCH` (annotated), pushed; the tagged tree is the
   reproducible snapshot the version names.

## Alternatives

- **ZeroVer forever / never tag**: avoids commitment but forfeits the
  checkpoint value; users and bug reports need a name for "the version I ran".
- **Start at 1.0 with editions**: honest projects don't 1.0 on day one; the
  stability machinery (a future RFC, far out) should be designed when there is
  something to keep stable.
- **CalVer**: communicates recency, not maturity; witchy's story is "early and
  moving", which 0.x states exactly.

## Drawbacks

- A public 0.x invites "is it ready?" — answered by the gate definition above.
- The changelog is a standing editorial duty; kept cheap by writing it at
  release time from the RFC/bug ledgers rather than reconstructing history.

## Prior art

- Rust pre-1.0 (0.x with aggressive breakage, then a deliberate 1.0),
  SemVer's 0.x clause (anything may change), and the repo's own
  break-don't-deprecate policy, which this RFC publishes rather than changes.
