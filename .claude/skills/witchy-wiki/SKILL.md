---
name: witchy-wiki
description: Build and maintain the LLM-owned knowledge wiki under wiki/. Use when asked to ingest a new source into the wiki, lint the wiki for staleness/contradictions, rebuild the wiki, or answer a question from the project's knowledge base. Adapts Karpathy's "LLM wiki" pattern over witchy's spec/ + rfcs/ + external-refs/ + code.
---

# witchy-wiki

You maintain `wiki/` — a cross-linked, browsable synthesis of witchy's knowledge.
You are a disciplined wiki maintainer, not a chatbot. Follow this exactly.

## Sources of truth (you read these; you NEVER edit them to match the wiki)

In priority order when they conflict:

1. **The code + tests** (`src/`, `std/`, `tests/`) — ultimate authority for "what IS".
2. **`spec/`** — the authoritative prose description of current behavior.
3. **`rfcs/`** — decisions and rationale ("why/when"). Note: an RFC may be stale
   vs. reality; defer to spec/code for current behavior, cite RFCs for *why*.
4. **`external-refs/`** — frozen external material; cite, never restate as ours.

`wiki/` is **derived and disposable**. If a wiki page disagrees with a source,
the page is wrong. Never reverse that.

## Page conventions

Every `wiki/` page is a markdown file with this frontmatter:

```
---
title: <page title>
commit: <git HEAD short-sha the page was synthesized against>
sources:                # every source that backs this page
  - spec/language.md
  - rfcs/0007-foo.md
  - src/typeck.rs
  - external-refs/karpathy-llm-wiki
updated: YYYY-MM-DD
---
```

- Link related pages with `[text](other-page.md)`. Every page should have at
  least one inbound link (no orphans) and link out to its neighbors.
- Every nontrivial claim cites a source. A claim with no source is a bug.
- Mark uncertainty inline: `^[inferred]` (synthesized, not stated by a source),
  `^[ambiguous]` (sources disagree or are unclear).

## Operations

### ingest `<source>`
Fold one new/changed source into the wiki. Do NOT just append a page.
1. Read the source fully.
2. Find every existing page it touches (grep titles, entities, concepts).
   Expect one source to touch several pages.
3. For each: **merge** — update claims, strengthen cross-references, add what's
   new. Never duplicate content that already has a home.
4. If the source contradicts an existing page, do NOT silently overwrite. Note
   both, mark `^[ambiguous]`, and resolve using the source-priority order above
   (code/spec win). Record the resolution.
5. Create new pages only for genuinely new entities/concepts.
6. Update `commit:`, `updated:`, and `sources:` on every page you touch.

### lint
Health-check the wiki. Report (don't auto-fix without confirmation) every:
1. **Stale page** — `commit:` lags `HEAD` and a `sources:` file changed since
   that commit (`git log <commit>..HEAD -- <source>`). These need re-ingest.
2. **Contradiction** — pages that disagree, or a page that disagrees with its
   cited source.
3. **Orphan** — a page with no inbound links.
4. **Unsourced claim** — a nontrivial statement with no backing source.
5. **Dangling link** — a link to a page that doesn't exist.

### rebuild
When drift is large (lint is mostly red), **regenerate — don't patch**:
1. Archive the current wiki to `wiki/_archive/<UTC-timestamp>/`.
2. Regenerate pages from the sources of truth, current as of `HEAD`.
3. This is the escape hatch. A fresh wiki beats a stale one you half-trust.
   Confirm with the user before a full rebuild (it's destructive to the live wiki).

### query `<question>`
Answer from the wiki + sources. If the answer required real synthesis and isn't
already a page, **file it back** as a new page (with sources) so the exploration
compounds. The wiki sits *alongside* search, not instead of it — for a question
the wiki can't answer, fall back to grepping the sources, then capture the result.

## The discipline (why this stays honest)

- **Commit-stamping** makes staleness visible and cheap to detect.
- **Source attribution** makes every claim auditable back to ground truth.
- **Regenerate-don't-maintain** caps drift: never let the wiki rot into something
  you can't trust. The maintenance cost being "low" is a *thesis*, not a law —
  the archive+rebuild hatch is the insurance.
