---
rfc: 0075
title: "Lead with the differentiator: the first-contact arc for README, book, and spec"
status: implemented
created: 2026-07-07
# Implemented across the docs truth/positioning wave:
# - 7fc65dab: spec/language.md quick index, sealed-types section, capability/right/verb vocabulary
# - a09192f3: Getting Started bridge that leads with capabilities
# - README.md now opens with the language/capability thesis before the package-security application
# - CONTRIBUTING.md records the capability/right/verb terminology rule for future docs/diagnostics
tracking: implemented on master (7fc65dab, a09192f3, README/CONTRIBUTING positioning and terminology)
related:
  - "0019 (interactive docs — superseded lineage; this is prose-arc, not tooling)"
  - "0063 §docs (release truth pass — owns claim accuracy; this owns *ordering*)"
  - "0065 (sealed type constructors — the construct the spec uses but never defines)"
  - "0067 story 6 (docs describe the shipped model)"
---

# RFC-0075: Lead with the differentiator

## Summary

A motivated stranger currently meets witchy in the wrong order. The README
spends its first ~96 lines on supply-chain context before saying what the
language is; the book teaches values, functions, data, errors, generics,
reflection, iterators, and comptime — eight chapters — before capabilities
appear (`book/src/SUMMARY.md:11-19`); the spec uses `sealed type` throughout
`stdlib.md` without ever defining it, and `spec/language.md` is a 1,500-line
monolith with no index. This RFC fixes the *arc*, not the content: a
capabilities-first bridge in chapter 1, a reordered README opening, a spec
quick-index, a sealed-types definition section, and one terminology rule.

## Motivation

The audit's docs reviewer read the front door cold and reported the
predictable evening: after 2–3 hours a reader can write witchy and has a
correct model of pure-vs-effectful code — but doesn't yet know *why witchy
exists*, because the differentiator arrives in the book's back half. The
Introduction promises "authority is a value" and then the next seven chapters
teach things Go and Rust also do. Readers who leave at chapter 4 leave with
"nice small language," which is a positioning failure the prose ordering
creates all by itself.

Independent smaller gaps compound it:

- `sealed type` appears ~5 times in the generated `spec/stdlib.md`
  (`Rng`, `Version`, `DateTime`, …) and in [`spec/capabilities.md`](../spec/capabilities.md), but no
  spec section defines the construct (RFC-0065 shipped it; the spec never
  caught up — the RFC is history, not reference, per rfcs/README.md's
  cardinal rule).
- `spec/language.md` has no table of contents; finding the trait or pattern
  sections means scrolling or grep.
- capability / right / verb are used interchangeably across README, book, and
  spec; the three words have three precise meanings the docs never pin.
- Generator re-run semantics — the one shipped semantic a Rust/Python reader
  will mispredict (state persists across `yield` because the body *re-runs*,
  not because a continuation is captured) — lives as an aside in the
  mutation-scope section ([`spec/language.md:524-526`](../spec/language.md)) instead of leading the
  generators section.

Doc *drift* items (stale claims, broken links) stay in the bug ledger and the
RFC-0063 truth pass; nothing here re-litigates content accuracy.

## Design

Five bounded edits:

1. **Book bridge, not a reorder.** A short section in Getting Started —
   "Why capabilities matter" — right after hello-world: the hello program's
   `main(console: Console)` *is* the model (can print because it holds
   Console; a function without it cannot), three sentences of contrast with
   ambient authority, and a forward pointer to the capabilities part. Each
   tour chapter that follows gets one referring sentence where natural (e.g.
   functions: "note the signature says what this function may *do*, not just
   its types"). Full chapter reordering is rejected — the tour chapters
   build on each other, and the bridge buys ~80% of the positioning for ~2%
   of the churn. All new ```witchy fences are complete runnable programs
   (they are executed tests).
2. **README opening reorder.** First screen answers, in order: what witchy is
   (one paragraph, plain words), the 30-second pitch with a capability
   signature as the hook, install/hello-world, *then* the supply-chain story
   as the flagship application of the model (content unchanged — it moves,
   it doesn't shrink). House rules apply: absolute claims, no comparative
   "beats Go" framing.
3. **Spec quick-index + sealed types.** A linked TOC at the top of
   `spec/language.md`; a new "Sealed types" section (syntax, the
   home-module-only construction rule, match/read unaffected, one example —
   RFC-0065's shipped semantics stated as present-tense reference); the
   generators section opens with the re-run model before the first example.
4. **Terminology rule.** One paragraph in CONTRIBUTING.md and a matching
   spec glossary line: **capability** = the unforgeable value; **right** =
   a permission parameter within one (`Dir[Read]`); **verb** = an operation
   checked against rights (`read`, `connect`). One sweep aligns existing
   prose (mechanical; generated files fixed at their source doc-comments).
5. **Where they land.** All prose "describes what IS" (house rule — no
   history, no migration framing).

## Alternatives

- **Full book reorder (capabilities as chapter 2)** — rejected for 0.1:
  every tour chapter's examples would need re-basing so they don't use
  not-yet-taught constructs; the bridge achieves the positioning without the
  rebase. Revisit post-0.1 if the bridge proves insufficient.
- **Do nothing until the 0063 truth pass** — rejected: the truth pass audits
  claim *accuracy*; nothing in it reorders the arc, and the two compose
  cleanly (this first, truth pass over the result).
- **Define sealed types only in the book** — rejected: the spec is the
  reference; stdlib.md already links readers there.

## Drawbacks

- README/book edits invalidate readers' muscle memory of section positions —
  trivial pre-release.
- The terminology sweep touches many files shallowly; it is grep-shaped and
  reviewable as one mechanical commit.
- Executed-fence discipline means the new Getting Started example must be a
  real program; slight authoring cost, real anti-drift benefit.

## Prior art

- Rust's book leads with ownership — the differentiator — by chapter 4, and
  its front page says "memory safety" before anything else; the lesson is
  that positioning lives in ordering.
- Oberon/E-lang capability literature consistently teaches "authority is a
  parameter" with the hello-example trick used in step 1.

## Verification

- book/spec ```witchy fences are executed by the docs test suite; the fmt
  gate covers touched `std/` doc-comments if the glossary edits reach them
  (`spec/stdlib.md` regenerates from source comments — never hand-edited).
- `./scripts/check.sh --fast` (docs tests included); full gate via the merge
  queue before landing.
