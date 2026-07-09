---
rfc: 0071
title: "Idiomatic witchy: the house style and the dogfood modernization cut"
status: planned
created: 2026-07-07
# Accepted 2026-07-08 with a split: the idiom canon (§1, §3's CONTRIBUTING rule)
# is effective immediately for all new witchy code; the mechanical sweep (§2)
# and the fmt-gate flip remain gated on RFC-0070 D8 (fmt round-trip fidelity,
# BUG-330/331/332) — fmt is the sweep vehicle and currently eats comments.
tracking: quality audit 2026-07-07 (scratch/audit-2026-07-07-quality/REPORT.md, F1/F2)
related:
  - "0022 (place-assignment / statement-form mutators — the idiom this teaches)"
  - "0044 (std error policy — extended here from std to projects/)"
  - "0067 (coherence map story 6: docs describe the shipped model — extended to: the dogfood demonstrates it)"
  - "0070 D8 (fmt round-trip fidelity — the enabling vehicle; sequence after it)"
---

# RFC-0071: Idiomatic witchy — the house style and the dogfood modernization cut

## Summary

The flagship witchy programs (`projects/pm`, `coven`, `coven-web`) are written
in a fossil dialect one generation older than the language that ships. This RFC
(1) defines the canonical idiom set in one executable book chapter plus a short
normative CONTRIBUTING section, and (2) executes a one-cut, behavior-preserving
modernization sweep of `projects/**`, after which `projects/**` joins the
`witchy fmt` gate so the dialect cannot fossilize again.

## Motivation

During the 2026-07-07 quality audit, a fresh-context reviewer read ~5,000 lines
of `projects/` code cold and confidently reported **six missing language
features — all of which exist, are specced, and are tested**:

| "Missing" per the dogfood | Actually shipped |
| --- | --- |
| error propagation (`?`) | `e?` / `e? "msg"` — `spec/language.md` §9 |
| string interpolation | `"${expr}"` is general syntax |
| `for (k, v) in pairs:` | `spec/language.md:397` |
| `fold`/`any`/`all`/`zip` | `std/iter.witchy` (+ eager `fold`/`any`/`all` on list) |
| list-append sugar | statement-form mutator: `xs.push(v)` *is* `xs = list.push(xs, v)` (RFC-0022) |
| `string.cut` | `string.split_once` |

The corpus taught the reviewer the wrong language. Measured on `pm.witchy`
(2,805 lines): **106** `+ "` concatenation sites vs **12** interpolations;
**47** `out = list.push(out, x)` reassignment ladders; **2** uses of `?`;
hand-rolled JSON field extractors (`pm.witchy:1395-1420`) duplicating
`std/json`'s shipped `get_string`/`get_int`/`*_of` helpers.

Worse than verbosity, the projects disagree on the load-bearing idiom —
error handling:

- **pm** returns `Option(String)` with **inverted polarity** (`Some` = error,
  e.g. `lock_integrity_error`, `pm.witchy:209`) — a shape `?` cannot compose
  with, which is *why* pm can't adopt `?`;
- **coven-web** uses nested match/if ladders 3–4 deep
  (`coven_web.witchy:479-493`);
- **glamour** encodes policy in sealed capabilities (the modern style).

Anyone — human or LLM — learning witchy from its own largest programs learns
`Some(msg)`-as-error and string concatenation. This is negative marketing for
the language's actual ergonomics, and it systematically poisons audits and
agents into re-proposing features that exist. RFC-0067's story 6 says the docs
must describe the shipped model; the dogfood is the docs people trust most.

## Design

### 1. The idiom canon (docs)

One new book chapter, `book/src/idioms.md` ("Idiomatic witchy"), with
**executed** examples (the existing ```witchy-fence test discipline is the
enforcement). It canonizes, with a before/after pair each:

1. **Interpolation over concatenation** — `"${host}:${port}"`, never
   `host + ":" + port`. Concatenation remains for byte-exact joining of large
   pieces.
2. **Statement-form mutators over reassignment** — `out.push(x)` /
   `d.insert(k, v)` / `d[k] = v`, never `out = list.push(out, x)` (RFC-0022).
3. **`Result(T, String)` + `?` for fallible flows** — per RFC-0044's rules,
   extended normatively from std to all repo witchy code: lookup miss →
   `Option`, invalid input / failed operation → `Result`, propagate with
   `e?` / `e? "context"`. **`Option(String)`-with-`Some`-as-error is banned**;
   it inverts polarity and blocks `?`.
4. **Combinators where they clarify** — `iter`/`list` `filter`/`map`/`fold`/
   `any`/`all` over index-threaded `while` loops; `for (a, b) in pairs:` over
   `let (a, b) = pair` in the body.
5. **std helpers over hand-rolls** — `json.get_string`/`*_of`,
   `string.split_once`, `list.contains` etc.; a private wrapper around a std
   function is a smell that the std function (or the caller's shape) is wrong —
   file it, don't wrap it.
6. **Method-call form** as the default spelling (already the documented form).
7. **Sealed capabilities for policy** — glamour's `UiRoot`/`UiFetch` pattern,
   held up as the positive exemplar.

Plus a matching ~20-line normative section in `CONTRIBUTING.md` ("Witchy code
in this repo follows the idiom canon — book/src/idioms.md") so the rule binds
contributors and agents, not just readers.

### 2. The dogfood cut (one sweep, no aliases)

A behavior-preserving modernization of `projects/pm`, `projects/coven`,
`projects/coven-web`, `projects/glamour`, `projects/docs` — in that order of
payoff (pm is 2,805 lines and the worst offender). Mechanical passes:

- concatenation → interpolation (excepting deliberate byte-joins);
- `x = list.push(x, e)` / `d = dict.insert(d, k, v)` → statement-form mutators;
- `Option(String)`-as-error signatures → `Result(Nil, String)` (or
  `Result(T, String)`), callers converted to `?` / `? "context"` chains;
- match/if error ladders → `?` chains where the semantics are identical;
- hand-rolled helpers that duplicate std (`signed_field_str`,
  `split_once_opt`-style wrappers, manual `while` scans) → std calls or
  combinators;
- `for pair in …: let (a, b) = pair` → `for (a, b) in …:`.

**Not in the sweep:** anything that changes behavior, output bytes, on-wire or
on-disk formats, or error *text* that tests assert on; restructuring that fights
known compiler constraints (e.g. pm's `main`/`dispatch_more` dispatcher split
exists to avoid a codegen stack overflow — `pm.witchy:74`; leave it).

### 3. The gate (so it sticks)

After the sweep, `projects/**/src/*.witchy` joins the `witchy fmt` check
(today `CLAUDE.md` explicitly excludes it — flip that line). The idiom rules
that fmt cannot enforce (error shapes, std-over-hand-roll) are enforced by the
CONTRIBUTING section and review.

### Sequencing

After **RFC-0070 D8** (fmt round-trip fidelity): fmt is the declared one-cut
vehicle, and today it eats comments (BUG-330–334), which disqualifies it from
sweeping 5,000 lines of commented code. If D8 slips, the sweep proceeds
hand-edited (it is mostly local rewrites) and only the *gate* step waits on D8.

### Verification

Every pass is gated by the standard machinery — the projects are e2e-tested,
so behavior preservation is checkable:

- iterate: `./scripts/check.sh --fast`; project shard: `--e2e` / `--examples`;
- full gate via `./scripts/merge-queue.sh submit <branch>` (never two full
  gates at once; the glamour publish e2e is load-flaky — verify in isolation);
- parity note: these are `.witchy` source edits, identical to both backends by
  construction; the differential suite still runs in the gate. `witchy <file>`
  runs the **compiled** backend only — use `witchy parity` if any output looks
  suspicious.
- landmines: never `cargo fmt` (Rust is hand-formatted); `spec/stdlib.md` is
  generated from `std/*.witchy` doc-comments — regenerate, never hand-edit; no
  new per-method optimization fast paths (`*_cap`/`self_*`) — irrelevant here
  but binding on any helper the sweep touches.

## Alternatives

- **Do nothing** — rejected: the corpus is actively teaching a wrong dialect,
  and the cost compounds with every reader and every agent session.
- **Docs only, no sweep** — rejected: readers weight real code over style
  chapters; the fossil corpus would keep contradicting the canon.
- **Sweep only pm** — a defensible 60%-of-value fallback if time-boxed, but the
  cross-project error-idiom disagreement (the F2 half) only closes if coven and
  coven-web move too.
- **A linter** — no witchy linter exists; building one for this is a bigger
  project than the sweep. The fmt gate + CONTRIBUTING rule is the cheap 90%.

## Drawbacks

- **Churn in shared files.** `projects/` sees concurrent agent work; the sweep
  must land in a worktree branch per project, coordinated via the merge queue,
  with `git status` checks before claiming files (per CLAUDE.md).
- **Diff noise** obscures blame history for these files — accepted;
  break-don't-deprecate is the standing pre-1.0 posture.
- **Risk of behavior drift** in subtle rewrites (`?` changes early-return
  points). Mitigation: the mechanical passes are reviewable one pattern at a
  time; the e2e suites are the behavior oracle; anything the sweep cannot
  rewrite pattern-locally stays as-is and gets a `// idiom-exempt: <why>`
  comment — and each such exemption is a data point that the idiom has a real
  gap (which is exactly the field test the language wants).

## Prior art

- Go's `gofmt`-then-`go fix` lineage: canonical style enforced mechanically,
  one-cut migrations at language transitions.
- Rust's edition idiom lints (`cargo fix --edition-idioms`): the compiler
  vendor modernizes its own corpus first.
- This repo's own Phase-2 "builtins become the stdlib — one cut, no aliases"
  (91f2122) proved the one-cut sweep works here at larger scale.
