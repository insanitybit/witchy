# Learner round 3 — evaluation, and the round-4 plan

Round 3 (scratch/LEARNING-LOG.md; rounds 1–2 archived at
~/workspace/witchy-scratch-round{1,2}) ran against the post-round-3 language:
statement arms, the prelude documented, derive/comptime in the book,
per-function caps. The learner wrote 23 programs + a 2-rune project + 4
probes; ~70% first-try success; parity clean everywhere it applies.

## Evaluation

**The language held; the seams didn't.** Zero language-level silent-wrong
findings and zero compile-loud gaps — every error was loud and informative.
But round 3 found the project's first true Blocker outside the compiler:
`witchy fmt` rewrote `let ys = [n * n for n in xs]` into `let ys = 0`. The
printer leaked its best-effort placeholder for the comprehension desugar, and
the idempotence-only round-trip check shipped it — a stably-mangled program
reproduces itself. The lesson is recorded in `format.rs`: **idempotence is a
stability check, not a semantics check.** `reformat` now compares input and
output ASTs (canonicalized), so a printer bug can downgrade fmt to a no-op
but can never ship a different program.

Fixed same-day, all verified by the suite + e2e:

1. **fmt Blocker** — comprehensions re-sugar everywhere (value position,
   tails, filters, multiple generators); the semantic round-trip guard is
   back (AST equality modulo line metadata + moved-builtin renames).
2. **`witchy run [args...]`** forwards argv to `main`'s `args` (with `--`
   respected, and `-C` never read past a `--`).
3. **`http` demands `Net[Connect, Tcp]`**, `server` demands
   `Net[Listen, Tcp]` — the book's narrowing recipe now composes; std's own
   footprint is honest about which verbs it uses.
4. **`witchy caps`** with no file inside a project analyzes the entry module.
5. **`without Dir:`** now hints: "`without` names the binding, not its type;
   did you mean `dir`?"
6. Docs: less-than-predicate convention (`sort_by`), hex-string hashes,
   `json.decode` → `Json` named in the appendix table.

## Round 4 — the loop graduates from fixing to expanding

### A. Surface: tuple field access — SHIPPED
`pair.0` / `pair.1` works on both backends (digits after a field-access dot
lex as indices, so `grid.0.1` chains); typed via the table, loud on arity and
non-tuple bases; `examples/tuples.witchy` pins it.

### B. Stdlib: the next Go-parity slice
Round 3's misses were all *discovery*, not absence — the functions existed
under proper names. So grow breadth where programs actually stalled:
- `time`: formatting/parsing (RFC3339 + a strftime-ish subset over `Clock`).
- `json`: `encode` ergonomics for records (derive-assisted `to_json`?
  comptime-able?) — the learner hand-built `Json` trees.
- `string`: `pad_left`/`pad_right`/`repeat` (the learner wrote all three by
  hand for table rendering).

### C. Tooling: discovery aids
- `witchy doc --search <name>` (or `witchy which split`): given a bare name,
  print the module-qualified candidates with signatures — the round-3 naming
  frictions (to_milliseconds, decode, sort_by) all die here.
- The unknown-function suggester already does closest-match; extend it to
  search across modules ("`to_ms`? `duration.to_milliseconds` exists").

### D. Round 5 setup
Re-run the learner after A–C with the same prompt. Target: a round whose
findings are all "worked well" or genuinely new feature territory.
