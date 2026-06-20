# external-refs/

Curated external material that informs witchy's design — papers (PDFs), notes on
external pages, prior-art writeups, talks. The bookmark-manager layer: dump a
source in, and the agent files it.

## Rules

- **Frozen sources.** An external ref is immutable. A paper doesn't change, so we
  never edit one to reflect our state — we only add notes *about* it. This is the
  whole reason this layer is cheap to keep correct: the inputs don't move.
- **Curated, not authoritative.** These sources informed decisions; they don't
  describe witchy. They feed [`rfcs/`](../rfcs/) and [`wiki/`](../wiki/), which
  cite them.
- **Index everything.** [`index.md`](./index.md) has one entry per ref: what it
  is, where it came from, why it matters, and what it informed (with links to the
  RFC/wiki pages that used it). The [`witchy-wiki`](../.claude/skills/witchy-wiki/SKILL.md)
  skill maintains the index as new refs land.

## Layout

```
external-refs/
  index.md          # the curated catalog (agent-maintained)
  <slug>/           # one dir per ref: the PDF/snapshot + a notes.md
```
