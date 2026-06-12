# Learner round 4 — evaluation, and the round-5 worklist

Round 4 (scratch/LEARNING-LOG.md; rounds 1–3 archived at
~/workspace/witchy-scratch-round{1,2,3}) ran against the post-round-4
language: tuple element access, `witchy which`, time parsing/formatting,
derive(Json). 21 programs + a two-rune project + 3 deliberate-error probes;
**20/22 first-try**; parity clean everywhere; **zero fmt-induced behavior
changes** across every file (the round-3 regression class is dead).

## Evaluation

One Blocker — and it was round 3's own `witchy run` fix: argv was passed into
`run_module`'s `net_allow` positional, so args were dropped AND
`witchy run host:port` would have *widened the program's reachable network*.
Fixed (run_project → `run_module_args` with a deny-all allowlist) and pinned
by a test the same day. Lesson recorded: a positional `Vec<String>` next to
another `Vec<String>` is exactly where the compiler can't help — the test,
not the type, is the guard there.

Zero language-level findings for the second round running. Every remaining
item is polish:

## Round 5 — small, ordered by learner pain

1. **`Duration` interpolation** prints raw milliseconds (`30000`), not `30s`.
   Decide: make `to_string`/`${}` of a Duration render `duration.human` form
   on both backends (probably right — it is a distinct type, so a distinct
   rendering is honest), or flag the default loudly in the book.
2. **`witchy which <module>`**: fall back to module names — `which time`
   should list the `time` module's exports instead of "no match".
3. **`string.to_int` doc wording**: "errors" → "aborts via `fail`" — in a
   language where `Result` is the other failure channel, the distinction is
   load-bearing. Sweep stdlib doc comments for the same ambiguity.
4. **fmt's opinionatedness documented**: getting-started-toolbox should say
   the formatter canonicalizes *forms* (interpolation over `<>` chains,
   `var` conventions, bare nullary ctors), not just layout.
5. **`--help` completeness**: the main usage block should list the PM
   commands (or point at `witchy coven`), and document `run [args...]`;
   `witchy run --help` is swallowed by the program's argv by design — say so.

Then: learner round 5, expecting a findings list that is all Worked-well.
