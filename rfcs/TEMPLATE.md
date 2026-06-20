---
rfc: NNNN
title: <short title>
status: proposed        # proposed | planned | implemented | rejected | superseded
created: YYYY-MM-DD
superseded-by:          # NNNN-slug, only if status: superseded
tracking:               # where implementation is tracked (commit/issue/PR), optional
---

# RFC-NNNN: <title>

## Summary

One paragraph: what this proposes, in plain terms.

## Motivation

What problem this solves and why it's worth doing. What goes wrong if we don't.

## Design

The actual proposal. Be concrete enough to implement from. Syntax, types, data
shapes, behavior, edge cases.

## Alternatives

What else was considered and why this won. ("Do nothing" is a valid alternative
to weigh.)

## Drawbacks

Honest costs: complexity, performance, migration, things this makes harder.

## Prior art

External sources that informed this (link to `external-refs/` entries).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
