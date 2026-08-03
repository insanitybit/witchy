---
rfc: 0113
title: Improve agent contribution throughput
status: implemented
created: 2026-08-02
tracking: "Implemented 2026-08-02 in docs/agile-agent-playbook.md and scripts/agent-check.sh."
---

# RFC-0113: Improve agent contribution throughput

> Implemented 2026-08-02. Added `docs/agile-agent-playbook.md` as the
> repository-wide contributor workflow source and `scripts/agent-check.sh` as the
> agent-focused focused-verification entrypoint.

## Summary

Create a lightweight, durable workflow that reduces time-to-green for agent work by
reducing cognitive load, clarifying code ownership boundaries, and making fast
verification the default path. This RFC defines a repository-owned process for
agent-led features and bug fixes without changing compiler semantics.

## Motivation

Recent work has shown that agents can stall on cross-module changes when they have to
discover architecture expectations from scattered files, rerun broad check suites, and
manually reconstruct shared conventions. The result is avoidable latency: slower progress,
more context churn, and inconsistent handoffs.

The objective is to make contributions faster and more predictable without weakening quality:

- keep edits narrowly scoped to well-defined ownership boundaries,
- make checks fast and targeted by default,
- preserve durable handoff artifacts so any agent can pick up work with minimal
relearning,
- keep merge-queue interaction clean and batchable.

## Design

This RFC proposes three layers: documentation, workflow, and local tooling.

### 1) Documentation: `docs/agile-agent-playbook.md` (new)

Add a short playbook with:

- accepted feature pipeline (`syntax -> rewrite/link -> typeck -> backend checks`),
- ownership map for common touchpoints (`syntax`, `keyword_args`, `typeck`, `lower`,
  `interp`, `spec`, `tests`),
- required artifacts per task (error examples, expected diagnostics, smoke tests),
- when to run targeted checks vs full gates,
- and examples of accepted vs rejected agent work (state transitions and handoff format).

The playbook is the single source of truth for agent-specific process before PR
submission.

### 2) Code organization guidelines for new features

Adopt a standard feature decomposition in the RFCs and implementation branches:

- **Parser/AST boundary** (`crates/witchy-syntax`): syntax and trees only.
- **Rewrite/link boundary** (`crates/witchy-syntax` and `crates/witchy-types`): rewrite and linkage
  normalization.
- **Diagnostics boundary** (`crates/witchy-types`): explicit error expectation and messages.
- **Interpreter/backend verification boundary** (`crates/witchy-interp`, `crates/witchy-lower`): behavior parity assumptions.
- **Spec/docs**: source of truth updates and migration notes.

For each boundary, RFCs must include at least one representative file set. The result is
smaller review diff surfaces and less cross-agent reasoning ambiguity.

### 3) Tooling and command defaults

Add a small script `./scripts/agent-check.sh` (or equivalent entry in `scripts/`):

- `./scripts/agent-check.sh target --package <name>`: run focused test shard
  (`env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= CARGO_TARGET_DIR=<target> cargo test -p <name> <filter>`),
- `./scripts/agent-check.sh paths <path-pattern>`: run `./scripts/test-for-paths.sh --run` for docs-only or
  path-limited changes,
- `./scripts/agent-check.sh syntax`, `./scripts/agent-check.sh link`, `./scripts/agent-check.sh parity` as
  named aliases for the most common shard families.

Default behavior: no command in this RFC runs full workspace checks. Focused validation
is mandatory, and full checks remain for coordinator gates and pre-submit review.

### 4) Merge-queue alignment

Adopt a rule for contributor agents:

- before editing: capture `git status --short --branch`, queue status, and branch name,
- after editing: run only the narrow checks needed by modified modules,
- submission: only then queue through `./scripts/merge-queue.sh submit`.

If a change is blocked by a full-gate dependency, queue the next disjoint unit immediately
instead of idling. This increases batching efficiency and improves merge throughput.

### 5) RFC update pattern

Every RFC that proposes a feature change must include:

- a boundary map of touched modules,
- one "minimal green" test command in the RFC body,
- explicit fallback if full checks are not run locally.

This aligns design docs with execution reality and prevents optimistic "implemented" states
that were only validated partially.

## Alternatives

- **Do nothing / rely on current AGENTS.md guidance.** This keeps flexibility but has
already been observed to reintroduce unnecessary re-reading, inconsistent checks, and
rework.
- **Mandate full `./scripts/check.sh` for every change.** This increases assurance but makes
agent feedback cycles too slow for parallel work.
- **Rely only on human conventions.** Already partially in place; effective for small teams,
  but insufficient for higher concurrency.

## Drawbacks

- adds a small process burden (extra docs and helper scripts),
- requires discipline to keep the playbook current,
- focused checks could miss global regressions if not paired with periodic full gates.

## Prior art

- Internal: [RFC-0079](./0079-queue-sharded-agent-fanout.md) (queue-driven contribution patterns),
- Internal: `scripts/MERGE-QUEUE.md` and `spec/architecture.md` (pipeline and serial gate contract),
- Internal: `AGENTS.md` instructions on shared worktrees, queue workflow, and focused gate strategy.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
-->
