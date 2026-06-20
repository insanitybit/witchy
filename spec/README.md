# spec/

The authoritative, always-current description of what witchy **is** today. If
`spec/` and the code disagree, one of them is a bug — they are meant to match.

## Rules

- **Describe what IS, never history.** No "we used to…", no "this replaces…", no
  diffs against past decisions. That belongs in [`rfcs/`](../rfcs/) and in commit
  messages. A reader of `spec/` should learn the current language, not its past.
- **No proposals.** Anything that isn't shipped-and-current goes to `rfcs/`.
- **Keep prose thin; anchor to runnable truth.** Prose drifts; tests don't.
  Where a spec section has examples, prefer examples that are actually executed
  by the test suite (e.g. `src/example_tests.rs`) so CI fails when the spec lies.
  Untestable prose is the danger zone — minimize it.
- **Stamp freshness.** A spec doc may carry `verified: <commit>` frontmatter
  recording the last commit its claims were checked against. The
  [`witchy-wiki`](../.claude/skills/witchy-wiki/SKILL.md) lint pass flags docs
  whose stamp lags far behind `HEAD`.

## What lives here

The current language reference, the stdlib reference, the capability model, the
runtime/architecture as it actually exists — the things a user or contributor
needs to know to use witchy *right now*.
