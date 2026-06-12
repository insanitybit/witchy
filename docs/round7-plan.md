# Learner round 6 — evaluation, and the round-7 worklist

Round 6 ran the morning after two one-cut migrations (`to_string` → `${...}`
and `<>` → `+`) with a brief that hammered string-heavy code specifically.
17 programs + a 2-rune project + 7 probes; 13/17 first-try; fmt idempotent
on every file; parity clean everywhere deterministic.

## Evaluation

**Both cuts held under targeted fire** — zero blockers, zero post-format
behavior changes, and the learner singled out the migration teaching errors
as a highlight. The remaining findings are seams, and the sharpest one is an
API-design lesson rather than a compiler bug:

1. **The unit trap (silent-wrong candidate, FIXED)** — `now(clock)` returns
   epoch *milliseconds*; `time.from_unix` takes *seconds*; composing them
   produced "the year 58416" with no error. Fixed by meeting the trap at
   both ends: `time.from_millis(ms)` exists (and is named as the idiom for
   `now`), `from_unix`'s doc names the failure mode, and the book teaches
   the unit at `now`'s first appearance. The standing lesson: **two
   prelude-grade APIs whose composition type-checks but is numerically
   wrong is a silent-wrong bug**, even with no compiler defect anywhere —
   audit unit boundaries (ms/s, bytes/chars, 0-/1-based) when designing
   std signatures.
2. **fmt rejected the documented `\$` escape (FIXED)** — the printer emitted
   `$` bare, the reprint re-parsed as interpolation, and the semantic guard
   refused to ship it. The guard did its job (refusal, not corruption);
   `string_lit` now escapes `$` so the form round-trips.

## Round 7 — remaining seams, ordered

1. **`or`/`and` diagnosis**: "unbound variable `or`" → suggest `||`/`&&`
   (a two-line hint in the unknown-variable path; Python habits are
   common).
2. **`let x: Duration = 1m`**: locals are inferred-only, but the parse error
   points at `=`. Either accept-and-check the annotation (witchy already
   has the types) or say "local bindings are inferred — drop the `: Type`".
   Decide; accepting is friendlier and costs one optional parser clause +
   a unify.
3. **`witchy sandbox` for projects**: `run` handles multi-rune projects,
   `sandbox` is single-file — the strongest confinement story stops at
   exactly the programs big enough to need it. Link the project the same
   way `run` does, then hand the linked module to the existing sandbox
   path.
4. **Scheduler message budget** (carried from round 5): audit the
   interpreter's 1M-message cap against the WASM actor host for the
   manufactured-divergence rule.
5. **Trait-dispatch reach** (carried): match-binder receivers in bounded
   generics, plus the targeted error.

Then learner round 7.
