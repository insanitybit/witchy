---
status: implemented
note: Historical planning doc, imported from docs/ under RFC-0001. Frozen — a record of what was planned, not current behavior (see spec/).
---

# Learner round 2 — evaluation, and the round-3 plan

> **STATUS (2026-06-12): sections A–C are SHIPPED.** Statement match arms /
> `let _` / `${}` string escapes; fmt canon (inline statement arms, if-let
> survives with else, bare nullary ctors) with the tree reformatted; `-C`
> position-independent for every subcommand; `witchy caps` prints per-function
> rows (the book's sample is now real output); `witchy new --lib`; `append` as
> a full Dir primitive on both backends. Book: prelude documented (+ imports
> dropped), derive section, comptime page, comprehensions, Dir verb table,
> generated-modules paragraph, caps/match/Inc fixes. Remaining: section D —
> learner round 4.

Round 2 (scratch/LEARNING-LOG.md; round 1 archived at
~/workspace/witchy-scratch-round1) ran against the post-evolution language:
typed lowering, value equality, the stdlib cut, methods/statics, derive,
comptime. The learner wrote 14 programs + 2 package projects + 7 probes.

## Evaluation: what changed between rounds

Round 1 produced two CRITICAL findings (silent `dict.get` divergence on the
sandbox; `${…}` codegen failures) and one structural learnability hole
(unguessable builtins). **Round 2 produced zero blockers, zero silent-wrong
findings, and parity passed on every program** — including derive, comptime,
actors, capabilities, and both package projects. Every remaining finding is
docs lag, surface ergonomics, or tooling consistency. The compiler stopped
being the problem; the book is now the bottleneck. (One round-2 finding is a
false negative that proves the point: the learner added `import dict`
everywhere because the book never says the prelude exists — the import is
unnecessary.)

## Round 3 — one cut each, ordered by learner pain

### A. Book catch-up (the dominant theme)

1. `derive(...)` section in tour-generics.md; a short `comptime:` page (with
   the determinism/additive story); comprehensions in tour-values.md.
2. Document THE PRELUDE prominently (modules chapter + first dict/list use):
   list/string/dict/math/option/result need no import line; drop the
   defensive imports from book examples and make them consistent.
3. packages-build.md: "where does generated code go?" — a build step's
   output files become NEW modules imported by name (`import generated`),
   not parts of the host rune.
4. capabilities chapter: a `Dir` verb table with semantics (`write` =
   overwrite); fix capabilities-optional.md's "append a line" comment.
5. capabilities-authority.md: stop overstating `witchy caps` output (or see
   C3 and make the output match the book).
6. match section: note the `arm -> expr` vs indented-statement-arm
   distinction (until B1 removes it).

### B. Surface ergonomics (small parser/lexer cuts)

1. **`->` match arms accept a statement** (`Some(e) -> out = list.push(out, e)`,
   `0 -> return Err("zero")`) — the indented form stays; the inline form
   stops being expression-only.
2. **`let _ = expr`** parses (evaluate, bind nothing).
3. **`\"` escapes inside `${…}`** — the interpolation scanner honors string
   escapes (re-enter string mode inside the braces).

### C. Canonical-form and tooling consistency

1. fmt vs book: `if let` must survive formatting (it already re-sugars the
   no-else shape; fix the else-carrying shape), and nullary constructors
   canonicalize to the bare form — update the book's `Inc()` to `Inc`.
   Rule: the formatter's output IS the canonical form; the book teaches it.
2. `-C` works for every project subcommand (`why`/`tree`/`outdated` included)
   — hoist it once at dispatch instead of per-command arg parsing.
3. `witchy caps --per-function` (or print per-function rows by default,
   matching the book's promise): the analyzer already computes per-function
   footprints; surface them.
4. `witchy new --lib` scaffolds `pub fn` + no `main`; `new`'s output mentions
   it.
5. `Dir` gains an `append` primitive (capability op, same confinement as
   `write`) — the auditor-log use case from the book needs it.

### D. Round 4 setup

Re-run the learner on a clean scratch/ after A–C, with the same prompt. The
target: a round where every finding is "worked well" or net-new feature
territory — at which point the loop graduates from fixing to expanding.
