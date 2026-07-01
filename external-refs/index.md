# External references — index

One entry per external source. Newest-relevant first.

## Knowledge-system design (informed RFC-0001 + the wiki/ design)

- **Generated wiki/source synthesis pattern** — gist `442a6bf555914893e9891c11519de94f`.
  The canonical statement of the three-layer pattern (immutable raw sources →
  generated wiki → schema doc) and the ingest/query/lint loop. The wiki we build
  is this pattern. *Caution:* the "maintenance cost is near zero" claim is an
  argued thesis, not a measured result. → informed `rfcs/0001-documentation-structure.md`, `wiki/`.
- **ar9av/obsidian-wiki** — github.com/ar9av/obsidian-wiki (MIT).
  The closest document-corpus implementation of the generated-wiki pattern:
  merge-based ingest, frontmatter source attribution, lint-for-staleness, and an
  archive+rebuild escape hatch. → informed the generated-doc maintenance workflow.
- **Cognition DeepWiki / "Ask Devin"** — docs.devin.ai/work-with-devin/deepwiki.
  Production wiki-plus-RAG over code corpora; staleness handled by periodic
  re-indexing. Evidence the materialized-wiki + retrieval hybrid runs in prod.
- **deepwiki-open** — github.com/AsyncFuncAI/deepwiki-open (MIT).
  Open reference pipeline: clone → embeddings → generated docs → Mermaid → wiki + Ask.

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

## Memory management & performance (informs performance-modes.md / ownership-analysis.md / regions.md)

The literature behind the "really solid default + drop-into-`opt`" memory model:
RC-as-floor with reuse, regions as an elision tier, escape analysis, and mutable
value semantics — plus the tracing-GC lineage we are choosing *not* to follow.

- **Perceus: Garbage Free Reference Counting with Reuse** — `perceus-2021/`
  (Reinking, Xie, de Moura, Leijen; PLDI 2021). Precise RC + reuse; cycle-free
  programs are garbage-free; enables FBIP. The canonical RC-as-floor reference —
  witchy's acyclic value heap is its exact precondition; the `__cap` token is a
  static special case of its reuse. → `rfcs/performance-modes.md` (tier 5; the
  "pick one memory identity" decision).
- **Counting Immutable Beans** — `counting-immutable-beans-2019/`
  (Ullrich, de Moura; IFL 2019). Lean 4's RC, precursor to Perceus; borrowed
  references + borrow inference keep RC traffic low. witchy's `let`-borrow ≈ their
  borrowed reference. → the RC-elision reading of the conventions.
- **FP²: Fully in-Place Functional Programming** — `fip-fully-in-place-2023/`
  (Lorenzen, Leijen, Swierstra; ICFP 2023). A linear calculus for when pure code
  runs with zero allocation (in-place) given unshared args. The static discipline
  `mode opt` would enforce. → `rfcs/performance-modes.md` tiers 3 + 5.
- **Implementation Strategies for Mutable Value Semantics** — `mutable-value-semantics-2022/`
  (Racordon, Shabalin, Zheng, Abrahams, Saeta; JOT 2022). The Hylo/Val basis —
  **witchy's design DNA**; `let`/`var`/`own` are Hylo's `let`/`inout`/`sink`. The
  authority for "no two bindings share mutable storage" → parity + acyclicity.
  *Caution:* not the arXiv 2106.12678 "Native Implementation" paper.
- **Region-Based Memory Management** — `region-based-memory-1997/`
  (Tofte, Talpin; Inf. & Comp. 1997). Stack of regions, allocation/free inferred
  by a type-and-effect system. Theory under `region:` + the watermark. *Caution:*
  pure regions leak long-lived values → keep regions a tier, not the whole story.
  → `rfcs/regions.md`; the escape/region lattice.
- **Escape Analysis for Java** — `escape-analysis-java-1999/`
  (Choi, Gupta, Serrano, Sreedhar, Midkiff; OOPSLA 1999). Connection-graph escape
  analysis for stack allocation + lock elision. Prior art for unifying witchy's
  six scattered escape computations into one lattice. → `rfcs/performance-modes.md`
  ("NEXT" lattice; tier 2 SROA).
- **C4: The Continuously Concurrent Compacting Collector** — `c4-concurrent-compacting-2011/`
  (Tene, Iyengar, Wolf; ISMM 2011). Load-barrier concurrent compaction, the ZGC
  ancestor. The tracing-GC path witchy is *not* taking (no cycles, no shared
  mutability ⇒ RC fits). **PDF gated behind ACM (403) — catalog-only.**
- **The Green Tea Garbage Collector** — `go-green-tea-gc-2025/` (notes only)
  (Knyszek, Clements; Go blog, Oct 2025; go.dev/blog/greenteagc). Page/span-oriented,
  SIMD-able marking to fix tracing's cache-hostile graph-flood (~90% of GC cost is
  marking, ≥35% stalled on memory). The locality tracing fights to recover is what
  bump/region allocation has for free — and RC has no mark phase at all.
- **ZGC — The Z Garbage Collector** — `zgc-openjdk/` (notes only; wiki JS-rendered)
  (OpenJDK; wiki.openjdk.org ZGC). Concurrent region-based compacting collector,
  colored pointers + load barriers, sub-ms pauses decoupled from heap size,
  generational since JDK 21. The "why RC, not tracing" counterweight — witchy's
  acyclic, unshared heap has none of the properties ZGC's machinery exists for.
