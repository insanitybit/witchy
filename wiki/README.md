# wiki/

An LLM-maintained, cross-linked, browsable synthesis over the authoritative
sources — the code, [`spec/`](../spec/), [`rfcs/`](../rfcs/), and
[`external-refs/`](../external-refs/). It exists so a human (or an agent) can
*explore* the project's knowledge by following links, instead of re-deriving it
from scratch with a search every time.

This is Karpathy's "LLM wiki" pattern: compile knowledge once into a compounding
artifact, rather than re-running retrieval on every question.

## What the wiki is NOT

- **Not authoritative.** The wiki is *derived* and *disposable*. If a wiki page
  disagrees with `spec/` or the code, the wiki is wrong — fix the wiki, never
  "fix" reality to match it.
- **Not hand-maintained.** You read it; the [`witchy-wiki`](../.claude/skills/witchy-wiki/SKILL.md)
  skill writes it. Don't hand-edit pages — your edits will be overwritten on the
  next rebuild and they break source attribution.

## Discipline that keeps it honest

- **Commit-stamping.** Every page records, in frontmatter, the source `commit:`
  it was synthesized from. That makes staleness *visible* — anyone can see a page
  was built against an old tree and regenerate it cheaply.
- **Source attribution.** Pages cite the `spec/`/`rfcs/`/`external-refs/`/code
  they were built from. A claim with no source is a bug.
- **Regenerate, don't maintain.** Small updates are merged in (ingest). When
  drift gets large, **archive the current wiki and rebuild from scratch** rather
  than patching — a stale wiki you half-trust is worse than a fresh one.

See the skill for the ingest / lint / rebuild operations.
