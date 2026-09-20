---
name: goal
description: Show, set, annotate, or close the active user goal that every agent in this repo works toward — the goal lives in `state/agents/GOAL-<slug>.md` (gitignored, observational). Use at the start of any session ("what is the goal?"), when the user states a new goal or changes priority ("the goal is now…", "/goal set …"), when recording progress against it, or when declaring it done.
---

# /goal — the active user goal

The briefing (`witchy-dev` §5) says "follow the active user goal and the live
merge queue" but never says where the goal is written down. This is where.
One file per goal under `state/agents/GOAL-<slug>.md`; exactly one is
`active` at a time. `state/` is gitignored and observational: a goal file is
not a lock and not file ownership.

## Usage

| form | effect |
|---|---|
| `/goal` | print the active goal (title, statement, acceptance, open items, last note) |
| `/goal set <slug>: <goal statement>` | create `GOAL-<slug>.md` as `active`; mark the previous active goal `superseded-by: <slug>` |
| `/goal note <text>` | append a dated line under **Progress** in the active goal |
| `/goal done` | mark the active goal `done` with the date and the master SHA that satisfied it |
| `/goal list` | every goal file with status and date |

Goal files written before this skill have no frontmatter; treat them as
`done` history and do not rewrite them.

## What a goal file contains

```markdown
---
status: active            # active | done | superseded
set: 2026-09-20
by: <user or agent>
superseded-by:            # slug, when status is superseded
---

# GOAL: <one sentence a reader can act on>

<Two to five lines: what "done" looks like and what it is not. Say what is
deliberately out of scope.>

## Ground truth first (every session, before selecting work)

1. `git status --short --branch` in your worktree; the main checkout stays on master.
2. `./scripts/merge-queue.sh status` and `./scripts/worktree-status.sh`.
3. The evidence documents the goal names (an RFC ledger, a handoff, a bug).
   Master and those documents win over this file if they disagree.

## Acceptance (evidence, not effort)

- <criterion> — evidence: <test / ledger row / command>, state: merged | queued | focused | open

## Out of scope

- <thing agents keep reaching for that is not this goal>

## Progress

- 2026-09-20: <dated, one line, exact SHAs>
```

## Rules

- **Exactly one active goal.** `/goal set` supersedes the previous one; do not
  keep two active files.
- **Acceptance rows are evidence-shaped.** A row names the test, ledger row,
  or command that proves it and its landing state. "Mostly done" is not a
  state.
- **Progress lines carry SHAs**, not adjectives. Merged means a `merged` event
  in `state/merge-queue/journal.jsonl`.
- **Do not self-select work off `bugs/README.md`** when a goal is active; the
  goal decides priority. If the goal is wrong, say so and ask — do not quietly
  replace it.
- When the goal changes the memory model, the language contract, or doctrine,
  the durable record is an RFC or an amendment, not this file.

## Reading the goal at session start

`ls -t state/agents/GOAL-*.md | head -3` and open the one whose frontmatter
says `active`. If none is active, the last `done` goal plus `git log -20
master` is the context; ask the user before inventing one.
