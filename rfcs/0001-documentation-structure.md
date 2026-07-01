---
rfc: 0001
title: Documentation structure — rfcs / spec / wiki / external-refs
status: implemented
created: 2026-06-19
superseded-by:
tracking:
---

# RFC-0001: Documentation structure

## Summary

Split the single `docs/` directory into four layers, separated by **mutability**
and **authority**:

| dir              | holds                                  | authority        | who writes | mutability |
|------------------|----------------------------------------|------------------|------------|------------|
| `rfcs/`          | proposals + decisions ("why/when")     | historical       | humans     | frozen once decided |
| `spec/`          | current state ("what IS today")        | authoritative    | humans     | always-current |
| `wiki/`          | cross-linked browsable synthesis       | derived          | generated  | regenerated |
| `external-refs/` | papers/notes/prior-art                 | curated input    | maintained | frozen sources |

The code + tests remain the ultimate authority for "what IS"; `spec/` is the
prose view of that, and CI is meant to keep them honest.

## Motivation

`docs/` conflates four different kinds of thing that have different lifecycles:
"things we might do", "things we did", "things we planned but did differently",
and "the current state". A reader can't tell which a given file is, and a writer
doesn't know where new material belongs. Concretely, today `docs/` contains
current language reference (`language.md`), shipped designs (`wir-design.md`),
in-flight designs, and dead roadmap (`round3-plan.md` … `round7-plan.md`) all in
one flat list.

Separating by lifecycle fixes this: each file's directory tells you what kind of
artifact it is and whether you can trust it as current.

## Design

The four layers, with the rule that makes each one cheap to keep correct:

- **`rfcs/`** — append-mostly. A decision is captured once and frozen; a changed
  decision is a *new* RFC that supersedes the old. The wrong turns stay visible
  instead of rotting. Cheap because immutability is the default. See
  [`rfcs/README.md`](./README.md).
- **`spec/`** — the only "always current" prose. Kept thin and, where possible,
  anchored to runnable examples so CI fails when it lies. The danger zone is
  untestable prose; minimize it. See [`spec/README.md`](../spec/README.md).
- **`wiki/`** — derived and disposable. Never hand-authoritative.
  Pages stamp the source commit they were built from; large drift triggers
  archive+rebuild, not patching. See [`wiki/README.md`](../wiki/README.md).
- **`external-refs/`** — frozen external sources plus a maintained index. The
  inputs don't move, so this layer is the cheapest of all to keep correct.

**The maintenance rule, in one line:** manual effort goes only into immutable
artifacts (writing an RFC, filing a ref). Everything mutable is either
executable-truth verified by CI (code/tests/spec examples) or
disposable-and-regenerated (wiki, derived prose). We never hand-edit something to
keep it true.

## Prior art

(Full notes in [`external-refs/index.md`](../external-refs/index.md).)

- **Generated wiki pattern** — the three-layer pattern (immutable sources →
  generated wiki → schema doc) and the ingest/lint loop. `wiki/` is this.
- **ar9av/obsidian-wiki** — a working document-corpus implementation we adapt the
  maintainer skill from.
- **Python PEPs / Rust RFCs / ADRs (Nygard)** — three independent, battle-tested
  precedents for the rfcs-vs-spec split. All freeze the decision and keep current
  state elsewhere; all warn the frozen proposal *will* drift from reality (which
  is exactly why `spec/` must exist separately).

**Cautionary tales we're heeding:**

- The "generated docs stay maintained at near-zero cost" promise is unproven.
  Treat `wiki/` as disposable and lean on commit-stamping + rebuild, not faith.
- Karpathy himself recommends wiki *alongside* retrieval, not replacing it.
- Even ADR authors abandon strict immutability for dated "living documents" —
  so `rfcs/` allows dated change-notes rather than pretending edits never happen.
- No tool combines a generated wiki *on top of* a formal rfcs/spec structure; that
  integration is ours to prove out.

## Triage — every current `docs/` file

Destinations below. **Basis** marks how the classification was reached:
`name` (from the filename/role) or `memory` (project memory about ship state).
Anything tagged *(verify)* must have its content confirmed when actually moved —
a doc can be part-spec, part-rfc and may need splitting.

### → `spec/` (current reference)

| file                  | proposed status | basis        |
|-----------------------|-----------------|--------------|
| `language.md`         | current         | name         |
| `stdlib.md`           | current         | name         |
| `capabilities.md`     | current         | name         |
| `capability-rights.md`| current         | name         |
| `architecture.md`     | current         | name         |
| `performance.md`      | current         | name         |
| `regions.md`          | current *(verify — may be a design)* | name |
| `binary-distribution.md` | current      | name         |
| `local-registry.md`   | current *(verify)* | name      |
| `ownership-analysis.md` | current *(verify — describes the shipped uniqueness pass)* | memory |

### → `rfcs/` (decisions, mostly already implemented)

| file                       | proposed status | basis  |
|----------------------------|-----------------|--------|
| `wir-design.md`            | implemented (WIR migration complete) | memory |
| `concurrency-design.md`    | implemented     | memory |
| `secrets-design.md`        | implemented     | memory |
| `package-manager.md`       | implemented (core) / planned (TUF phase B) *(verify split)* | memory |
| `performance-modes.md`     | proposed *(verify ship state)* | name |
| `language-evolution.md`    | implemented (historical roadmap) | memory |
| `coven-namespaces-plan.md` | proposed/planned *(verify)* | name |
| `oracle-only-migration.md` | implemented     | memory |
| `build-time-execution-plan.md` | implemented (historical) | name |
| `round3-plan.md`           | implemented (historical) | name |
| `round4-plan.md`           | implemented (historical) | name |
| `round5-plan.md`           | implemented (historical) | name |
| `round6-plan.md`           | implemented (historical) | name |
| `round7-plan.md`           | implemented (historical) | name |

## Migration plan

1. **Now (this RFC):** scaffold the four dirs + conventions + the maintainer
   skill, and migrate only the unambiguous, clean, historical clutter — the round
   plans + `build-time-execution-plan.md` — to prove the structure.
2. **On greenlight:** migrate the `spec/`-bound reference docs and the remaining
   `rfcs/`-bound designs. Several are under active modification in the working
   tree right now, so they're deliberately left until they're not mid-edit, to
   avoid clobbering concurrent work.
3. **Per doc on move:** confirm the *(verify)* classifications, split any doc that
   is part-spec/part-rfc, and add the appropriate frontmatter.

## Drawbacks

- A flat `docs/` is simpler than four dirs; this only pays off because the
  conflation is already causing real confusion.
- Triage of ~24 files is real work and some need splitting.
- `wiki/` adds a maintained artifact whose value depends on the (unproven)
  cheap-maintenance thesis — mitigated by treating it as disposable.

---

> **2026-06-20 — full migration executed.** All 24 `docs/` files were relocated
> (`docs/` removed); ~24 path tokens were rewritten across 39 main-tree files
> (README, CONTRIBUTING, `book/`, `justfile`, source comments, and the
> `spec/stdlib.md` test/regen path in `src/example_tests.rs` + `justfile`). The
> `stdlib_docs_are_current` test passes at the new path.
>
> Three `(verify)` docs landed in `rfcs/` rather than the `spec/` cell proposed
> above, on inspection: **`capability-rights.md`** (it is a *design* doc — the
> user-facing model lives in `spec/capabilities.md`), **`regions.md`** and
> **`ownership-analysis.md`** (both phased *design/analysis* records of shipped
> machinery, not current-state reference). `local-registry.md` stayed `spec/`
> (it's a how-to). No docs were split; any genuinely mixed doc can be split in a
> follow-up RFC. Temporary worktree copies were deliberately not
> touched.
