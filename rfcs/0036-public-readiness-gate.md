---
rfc: 0036
title: Public readiness gate — docs, terminology, and release hygiene
status: proposed
created: 2026-07-01
tracking: "Pre-open-source polish gate for making the repository coherent to a first-time reader."
---

# RFC-0036: Public readiness gate — docs, terminology, and release hygiene

## Summary

Before `witchy` is treated as public-facing, the repository gets a small, explicit
readiness gate. The gate does not try to make the project production-stable. It
makes the public surface coherent: current-state docs say what exists, RFCs say
what is proposed, examples run, generated files are labeled, and repository prose
uses intentional language instead of private shorthand.

## Motivation

The project already has a strong technical thesis, but a first-time reader will
judge the repository by the rough edges they see first: stale comments, proposal
language in current docs, project-specific shorthand, and unclear status markers.
Those problems make even good engineering look accidental.

The goal is not to sand away the project’s honest instability warnings. The goal
is to make every warning, caveat, and TODO look deliberately owned.

## Design

### 1. Current-state docs vs. design history

Every public-facing doc is classified into one of these buckets:

| Bucket | Location | Rule |
|---|---|---|
| Current behavior | `README.md`, `book/`, `spec/`, project READMEs | Must describe what works now. No roadmap language unless explicitly marked. |
| Design history / proposals | `rfcs/` | May discuss rejected paths and future work. Must not be the only description of shipped behavior. |
| Generated output | `spec/stdlib.md`, generated reports | Must say how to regenerate and what source owns it. |
| Local/dev notes | scripts, harness docs | Must be actionable and not depend on private context. |

### 2. Language quality pass

Repository prose should follow these rules:

- Prefer specific caveats over apologies.
- Avoid private coordination notes, conversational asides, and stale personal
  workflow references.
- Keep instability warnings near the top of user-facing docs, but state them as
  product status, not as throwaway commentary.
- Use the project’s canonical names consistently: `witchy`, `coven`, `rune`,
  `capability`, `footprint`, `grant`, `parity`.

### 3. Verification gate

The public-readiness gate is a scriptable checklist:

1. `./scripts/check.sh --fast` for build, lint, and non-flaky tests.
2. `cargo test documentation_examples_are_valid --workspace` for runnable docs.
3. A parity sweep over example entries before any public tag.
4. `witchy doc std/*.witchy > spec/stdlib.md` after stdlib doc-comment edits.
5. A text scan for private coordination wording before release notes are cut.

The scan is intentionally simple. It should catch words that usually indicate
private context rather than public documentation, then reviewers decide whether a
hit is legitimate, such as `User-Agent` in HTTP documentation.

### 4. Public issue labels / work buckets

Remaining polish work is tracked as visible buckets:

- `docs-currentness`: docs disagree with implemented behavior.
- `docs-language`: unclear, sloppy, or over-broad wording.
- `examples`: examples fail, are misleading, or lack quickstarts.
- `public-api`: CLI or package docs need first-time-user clarity.
- `security-posture`: security claims or caveats need precise wording.

## Alternatives

- **Do nothing.** Fastest, but leaves first impressions to chance.
- **Rewrite all docs before opening.** Too broad and likely to create churn. The
  gate should find and bound rough edges, not block on a perfect book.
- **Hide instability warnings.** Rejected. The warnings are part of the project’s
  integrity; they just need to be precise.

## Drawbacks

This adds review process to a young project. Some scans will false-positive, and
some docs will be temporarily labeled as rough. That is acceptable: visible status
beats accidental incoherence.
