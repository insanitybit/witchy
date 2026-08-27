---
name: witchy-docs
description: Write and review witchy's user-facing documentation — book/, spec/, README, and doc-comments in std/*.witchy. Use when adding or editing a book chapter, documenting a new language feature, auditing docs for staleness or devlog leakage, or when prose needs to read like documentation rather than a status report. Covers what may never appear in user docs, the executed-vs-static code-block trap, and the measured house voice.
---

# Writing witchy's user-facing documentation

This covers `book/`, `spec/`, `README.md`, and the doc-comments in `std/*.witchy`
that generate `spec/stdlib.md`. It does **not** cover `wiki/` (see
`/witchy-wiki`), `rfcs/`, or `bugs/` — those are internal artifacts with
different rules, and the distinction is the whole point of §1.

## 0. Know which file you're in

| Path | What it is | Editing rule |
|---|---|---|
| `book/src/*.md` | the narrative tour | edit directly; ordered by `SUMMARY.md` |
| `spec/language.md`, `spec/capabilities.md` | authoritative reference | edit directly |
| `spec/stdlib.md` | **generated** | never hand-edit — fix the `std/*.witchy` doc-comment, then `witchy doc std/*.witchy > spec/stdlib.md` |
| `book/examples.json` | **generated** test metadata | regenerate with `BLESS_EXAMPLES=1`, never hand-edit |
| `wiki/`, `rfcs/`, `bugs/`, `security-eval/` | internal | not user docs; different skills/conventions |

`stdlib_docs_are_current` fails if `spec/stdlib.md` drifts from the
doc-comments.

## 1. The prime rule: documentation describes what IS

A reader has no access to your development history and no reason to care about
it. Documentation states how the language works today. It is never a diff
against the past, a status report, or a record of who decided what.

**Never appears in user-facing docs:**

- **RFC numbers or `rfcs/` links.** "RFC-0122 adds explicit references" means
  nothing to a reader. Say what the feature does. (The one legitimate exception
  is a genuine *external* standard — `RFC-1918` private address ranges, `RFC-7519`
  JWT. Check before deleting.)
- **Acceptance ledgers, criterion maps, evidence tables.** A table mapping
  "criterion 14 → book section" is project management, not documentation.
- **Internal paths**: `scratch/`, `bugs/`, `security-eval/`, `state/`,
  `PRODUCT-STATUS.md`, `RELEASE-READINESS.md`, `bench/BASELINE.md`.
  Linking `crates/` or `spec/architecture.md` is fine when the reader would
  genuinely want the source.
- **Repo-process voice**: "the merge queue", "the acceptance gate", "the full
  gate", "landed in", "tracked in". The user's *own* CI and the coven publishing
  gate are real user-facing things — those stay.
- **Status hedges**: "currently implemented", "not yet supported", "in the
  current model", "planned", "remains unsupported until". State the rule. If a
  boundary is real and permanent, describe it as a boundary ("capabilities as
  `Dict` keys are rejected, because their layout isn't known until run time"),
  not as a temporary condition.
- **Migration tables for retired syntax.** Documenting "old spelling → new
  spelling" requires documenting syntax the language no longer has. Per
  *break, don't deprecate*, that history leaves the docs even when a migration
  command still exists in the CLI.
- **"The repository's ..."** — you are writing *for* someone using witchy, not
  *about* the witchy repo. `examples/minigrep does X`, not *the repository's*
  `examples/minigrep` does X.

**Quick sweep** (run from `book/src/` or `spec/`):

```sh
grep -rEn 'RFC[- ]?0[0-9]{3}|acceptance|criterion|ledger|scratch/|rfcs/|bugs/|PRODUCT-STATUS|RELEASE-READINESS|bench/BASELINE|[Tt]he repository|merge queue|currently implemented|not yet|in the current' *.md
```

Read every hit — `RFC-1918`, a user's CI, and "compiled-backend acceptance" are
legitimate. **Run it from the right directory**; a `cd` that silently fails will
sweep the wrong tree and report a false CLEAN.

## 2. The executed-block trap

This is the highest-value thing on this page.

- ` ```witchy ` blocks in `book/`, `README`, and `spec/` **are executed as
  tests**. Each must be a complete, correct program. A following ` ```text `
  block is its expected stdout.
- ` ```witchy-static `, ` ```sh `, and untagged blocks are **never executed**.

So **static blocks rot silently and no suite will ever tell you.** A chapter can
ship signatures that do not compile, for years, while the test suite stays green.

**When you touch a chapter, hand-verify its static blocks.** Extract anything
that looks like a complete declaration and check it:

```sh
./target/debug/witchy check /path/to/scratchpad/probe.witchy
```

Prefer converting a static block into an executed ` ```witchy ` block whenever it
can be made into a complete program — that is the only durable fix. Use
`witchy-static` only for genuine fragments (bare type spellings, signature
tables, partial snippets).

The same trap applies to **prose**: the build catches wrong code, never a wrong
claim. A documented API that never existed, or a project renamed since the text
was written, passes every test. When you touch a paragraph, verify its factual
claims against the tree — names, flags, paths.

## 3. Voice: the Rust Book, measured

The house target is the Rust Book's register: second person, example-first,
verbs over abstract nouns, and the concrete case before the general rule.

Measured against 8 Rust Book ownership/lifetime/error chapters (~21k words):

| Metric | Rust Book | Target |
|---|---|---:|
| `you`/`your` per 1k words | 9.7 | match it |
| mean sentence length | 22 words | 17–22 |
| em-dashes in prose | rare | **0** — witchy's docs use a spaced hyphen ` - ` |

The cookbook chapters (`cookbook-*.md`) already hit this and are the in-repo
reference for what good looks like. The failure mode at the other end is
**spec-voice**: impersonal, noun-heavy, rule-before-example. If a chapter has
zero instances of "you" across a thousand words, it is a specification, not
documentation.

**Do not chase a proxy metric.** A nominalization counter will flag `reference`,
`function`, `capability`, and `projection` — the subject nouns of the chapter,
not stylistic bloat. Measure to *find* candidates, then read the prose and judge.
Never rewrite to move a number.

## 4. AI-writing tells

Documentation that reads as machine-generated loses reader trust. The tells that
actually matter here, in rough order:

- **Negative parallelism** — "It's not X, it's Y", "not just X but Y". Once in a
  chapter is fine and often clarifying; a habit of it is the single most
  recognized tell.
- **Em-dash overuse** — witchy's docs use ` - `. Keep prose at zero em-dashes,
  code comments included, for consistency.
- **Rule-of-three padding** — stacked tricolons where two items or one would do.
- **Short punchy fragments** as standalone paragraphs for manufactured emphasis.
- **Grandiose stakes** — a chapter about `Dict` does not "fundamentally
  transform how you think about data".
- **Preamble** — "In this chapter, we'll explore..." Open with the thing itself.
- **Vocabulary**: delve, leverage (verb), robust, streamline, harness, utilize,
  seamless, crucial, pivotal, unlock, empower.

Detector for the mechanical ones:

```sh
grep -rEni "\b(delve|leverag|robust|streamlin|utiliz|seamless|crucial|pivotal)\b|—|it'?s not [^.;]{2,40}[,;] it'?s" *.md
```

Note the standing rule that **no comparative framing** belongs in public docs —
"beats Go" is internal shorthand only.

## 5. Add docs with the feature

For anything user-visible, a runnable `book/` example ships alongside the
differential test. Docs are not a follow-up task.

- New stable language/stdlib behavior → an executed ` ```witchy ` example **and**
  an independent expected result in `src/example_tests.rs`.
- Changed a `std/*.witchy` doc-comment → regenerate `spec/stdlib.md`.
- Invalidated generated evidence → regenerate it in the same change.

## 6. Verify

```sh
# the executed book/spec/README examples (~10 min warm; slower under agent load)
cargo nextest run -E 'test(/^example_tests::/)'

# regenerate example metadata after adding/removing/reordering a witchy block
BLESS_EXAMPLES=1 cargo nextest run -E 'test(/^example_tests::/)'

# stdlib doc drift
cargo nextest run -E 'test(stdlib_docs_are_current)'
```

`book/examples.json` keys on file + block index, so editing *comment text*
inside an existing block is safe, while adding, removing, or reordering blocks
requires a re-bless.

For a docs-only branch `./scripts/test-for-paths.sh` picks the right shard;
land through `/land` like anything else.

## 7. When docs and reality disagree

Source of truth order: `spec/` + `std/*.witchy` + independent expected results
(`src/example_tests/`, `tests/misc/semantic_conformance.rs`). The interpreter is
a source-level oracle, not the spec.

If verifying a doc claim uncovers a compiler defect — a form that should work but
does not, or worse, one where the interpreter and compiled WASM disagree — **file
it in `bugs/` and keep the docs honest.** Do not quietly reshape the
documentation to match a bug, and do not fix a parity-sensitive compiler defect
on a docs branch. A silent interpreter/WASM divergence is the most serious class
of bug in this project; `witchy parity <file>` is how you confirm one.
