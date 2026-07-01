---
rfc: 0038
title: Glamour and coven-web readiness — product polish for browser-facing demos
status: proposed
created: 2026-07-01
tracking: "Readiness plan for making glamour, coven-web, and the browser runtime look intentional and human-facing."
---

# RFC-0038: Glamour and coven-web readiness — product polish for browser-facing demos

## Summary

`glamour`, `coven-web`, and the browser runtime are the most visual parts of the
project. They should demonstrate witchy’s capability story without looking like a
pile of experiments. This RFC defines the readiness work needed to make those
subprojects presentable: clear demo boundaries, consistent terminology, verified
build/run paths, and product-quality copy.

## Motivation

A browser demo shapes first impressions faster than compiler internals do. If the
UI works but the docs are vague, the demo looks accidental. If the docs promise a
framework while the code is a prototype, the project looks over-sold. The right
presentation is: this is a capability-secure browser experiment with a small,
verified runtime boundary and a clear path to production hardening.

## Design

### 1. Project boundaries

The browser-facing work is documented as three layers:

| Layer | Owns | Public promise |
|---|---|---|
| `web/witchy-runtime` | JS host runtime, DOM adapter, tests | Runs compiled witchy wasm in the browser harness. |
| `projects/glamour` | Witchy UI library and examples | Provides a typed VNode/update model for demos. |
| `projects/coven-web` | Registry UI application | Demonstrates package browsing and capability-footprint presentation. |

Each layer gets a short README/status note that says whether it is a demo,
prototype, or supported surface.

### 2. Demo vs. product wording

Docs must not imply production readiness until these are true:

- browser tests cover routing, keyed updates, XSS-sensitive rendering, markdown,
  package pages, and HTTP effects;
- the build path is documented from a clean checkout;
- CSP / host authority boundaries are documented;
- examples avoid placeholder copy and unexplained screenshots;
- coven-web has a deterministic seed/demo data story.

Until then, wording uses **demo**, **prototype**, or **experimental**, not
**production-ready framework**.

### 3. Verification commands

Browser-facing PRs should document and prefer narrow checks:

- `node web/witchy-runtime/<test>.mjs` for targeted runtime tests.
- `bash web/witchy-runtime/demo/build.sh` for the demo bundle.
- `projects/coven-web/web/build.sh` for the coven-web frontend when dependencies
  are available.
- `git diff --check` for docs-only cleanup.

Each README should list the checks that actually work from a clean checkout.

### 4. Visual polish backlog

Before presenting the browser work as a public demo:

- Replace placeholder labels with concrete package/capability language.
- Make empty/error/loading states intentional.
- Keep screenshots current or remove them.
- Ensure package cards explain why capability footprints matter.
- Make failure states safe and boring: no raw HTML injection, no secret-bearing
  debug dumps, no unexplained stack traces in the user path.

## Alternatives

- **Hide the browser work.** Rejected. It is a strong demonstration of dev/deploy
  parity and capability-host boundaries.
- **Call Glamour a framework now.** Rejected. That over-promises. It is better to
  show a credible experimental UI layer.
- **Polish visuals before docs.** Rejected. Visual polish without honest status
  language can make prototype gaps look deceptive.

## Drawbacks

This RFC adds presentation work that is not compiler work. That is the point:
public credibility depends on the path from README to demo being coherent, not
only on backend correctness.
