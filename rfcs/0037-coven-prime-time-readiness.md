---
rfc: 0037
title: Coven prime-time readiness — registry, package manager, and trust flows
status: proposed
created: 2026-07-01
tracking: "Readiness plan for making coven understandable and credible as a public package-registry story."
---

# RFC-0037: Coven prime-time readiness — registry, package manager, and trust flows

## Summary

`coven` should become the clearest proof that witchy’s capability model matters:
package code, build-time execution, publishing identity, and dependency updates
all have visible authority boundaries. This RFC defines the remaining readiness
work needed before presenting `coven` as a prime-time subsystem.

## Motivation

The registry story is one of witchy’s strongest differentiators, but it spans many
moving pieces: local registry demos, trusted publishing, staged release, lockfiles,
capability footprints, widening gates, and self-hosted witchy programs. If those
pieces are documented as a pile of mechanisms, the story looks incoherent. If they
are documented as one trust pipeline, the project looks intentional.

## Design

### 1. Canonical user journeys

Coven documentation should center four journeys, each with commands and expected
output:

1. Create a rune and run it locally.
2. Publish to a local registry, stage it, promote it, then consume it.
3. Update a dependency whose footprint does not widen.
4. Attempt an update whose footprint widens and observe the block/approval path.

Each journey should have a single canonical source. Other docs link to that source
instead of restating the flow.

### 2. Trust vocabulary

Coven docs use these terms consistently:

- **rune**: a witchy package.
- **stage**: upload a signed candidate that is not yet released.
- **promote**: mark a staged candidate as released after human approval.
- **footprint**: capability authority recomputed from source.
- **widening**: any update that demands more authority than the locked version.
- **trust policy**: the identity rule for trusted publishing.

### 3. Implementation/status table

The Coven docs get one status table that marks each feature as:

| Status | Meaning |
|---|---|
| Built | Exercised by tests or demo scripts. |
| Prototype | Runs locally but still has known trust/UX gaps. |
| Planned | Designed in RFCs but not implemented. |
| Out of scope | Deliberately not part of Coven. |

The table prevents roadmap language from leaking into current-state docs.

### 4. Verification matrix

A Coven PR that changes behavior or docs runs the narrowest relevant check:

- `cargo test --test e2e` for package-manager lifecycle behavior.
- `./scripts/local-registry-demo.sh` for the happy-path demo.
- `witchy fmt --check projects/coven/src/*.witchy examples/coven_check/src/*.witchy`
  when `.witchy` files change.
- `git diff --check` for docs/comment-only changes.

### 5. Public polish checklist

Before calling Coven prime-time ready:

- Every public command in the docs is copy/pasteable from the repository root.
- Failure examples name the exact trust boundary that rejected the operation.
- Demo data is small, deterministic, and explained.
- No doc depends on private local state, unpublished services, or hidden accounts.
- The package-manager threat model links to the runnable demos that exercise it.

## Alternatives

- **Document every internal module.** Rejected for first public release; journeys
  matter more than internals.
- **Present Coven as production-ready immediately.** Rejected. The right target is
  credible prototype with precise boundaries.
- **Defer registry docs until implementation is complete.** Rejected. The registry
  is central to the language thesis and should be understandable early.

## Drawbacks

This may expose that some flows are prototype-quality. That is useful: public
credibility improves when the project names its gaps instead of implying more than
it has built.
