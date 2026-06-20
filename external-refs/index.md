# External references — index

One entry per external source. Newest-relevant first. The `witchy-wiki` skill
keeps this current as refs are added.

## Knowledge-system design (informed RFC-0001 + the wiki/ design)

- **Karpathy, "LLM Wiki"** — gist `442a6bf555914893e9891c11519de94f`.
  The canonical statement of the three-layer pattern (immutable raw sources →
  LLM-owned wiki → schema doc) and the ingest/query/lint loop. The wiki we build
  is this pattern. *Caution:* the "maintenance cost is near zero" claim is an
  argued thesis, not a measured result. → informed `rfcs/0001-documentation-structure.md`, `wiki/`.
- **ar9av/obsidian-wiki** — github.com/ar9av/obsidian-wiki (MIT).
  The closest document-corpus implementation of the Karpathy pattern: merge-based
  ingest, frontmatter source attribution, lint-for-staleness, and an
  archive+rebuild escape hatch. Our `witchy-wiki` skill is adapted from its
  `.skills/` skeleton. → informed `.claude/skills/witchy-wiki/`.
- **Cognition DeepWiki / "Ask Devin"** — docs.devin.ai/work-with-devin/deepwiki.
  Production wiki-plus-RAG over code corpora; staleness handled by periodic
  re-indexing. Evidence the materialized-wiki + retrieval hybrid runs in prod.
- **AsyncFuncAI/deepwiki-open** — github.com/AsyncFuncAI/deepwiki-open (MIT).
  Open reference pipeline: clone → embeddings → AI doc-gen → Mermaid → wiki + Ask.

## Decision-record / spec separation (informed rfcs/ + spec/)

- **Python PEP 1** — peps.python.org/pep-0001/.
  Status lifecycle as an enumerated header field; resolved PEPs become historical
  documents while current behavior lives in the Language/Library Reference.
- **Rust RFC book** — rust-lang.github.io/rfcs/.
  "We cannot expect every merged RFC to actually reflect what the end result will
  be." Accepted RFCs are frozen; substantial changes become new RFCs. The direct
  basis for our "an RFC is not the spec" rule.
- **ADRs (Nygard 2011) + adr.github.io + joelparkerhenderson/architecture-decision-record.**
  Immutable, append-only decision records; superseded-not-edited. Also the
  cautionary tale: in practice teams relax immutability toward dated
  "living documents" — which is why our freeze rule allows dated change-notes.
