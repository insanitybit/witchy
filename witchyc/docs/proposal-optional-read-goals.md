# Proposal: optional goals for slice reads

Status: proposed 2026-11-18 (late night). The binary-search case is done another way the same
night: `slice_index` recognises the midpoint `lo + (hi - lo) / 2` or `(lo + hi) / 2` under facts
`lo < hi` and `hi <= length` and asks the goal, which `omega` always shows
(`slices/binary-search-*`). The general mechanism below is still open.

## The gap

In a function with `proof: auto`, a slice read `s[i]` loses its test only when the path already
says `i < s.len()` in so many words (`scalar_vc/expressions.rs` `slice_index`). A goal is never made from a
guess, because a goal Lean cannot prove refuses the whole function: a program that compiles today,
with its test, would stop compiling.

Binary search is the case that matters (`lower_bound`, measured): every `+ - /` in it is proved,
but `s[mid]` keeps its test, because `mid < s.len()` follows from `lo < hi`, `hi <= s.len()` and
`mid = lo + (hi - lo) / 2` only by reasoning, which `omega` does and the syntactic test does not.

## The design

1. **Optional goals.** For each tested slice read in a proved function that the syntactic rule
   does not cover, `slice_index` records the goal `index < length` with its hypotheses as an
   optional obligation instead of nothing. One optional goal per function, the conjunction of its
   uncovered reads.
2. **After the required proofs succeed**, the build checks each function's optional goal on its
   own. Proved: the function's reads are all covered and `SLICE_PROVED` is set. Not proved: nothing
   changes; the reads keep their tests, and no diagnostic is shown.
3. **Caching** as for any goal (failures are cached too: a refused fixture rebuilds in 0.1 s), so a
   rebuild pays nothing.

## What must be settled first

- `proof::check_prepared` adds the program-wide goals and records model and audit identities; the
  session validates publication after the build. An optional pass must not change what the
  receipt says was proved for the required goals, and a failed optional goal must not count as a
  refusal anywhere (`proof_session` `refuse`, `LAST_FAILURE`). This is the trust-sensitive part.
- First-build cost: one more Lean request per function with uncovered reads (seconds, once).
- The trap report and `traps::nopanic` must read the final `SLICE_PROVED`.

## Tests to write first

`lower_bound` keeps its test today and loses it with this; a function whose read is genuinely out
of range (`s[s.len()]` under `if s.len() > 0`) still compiles, keeps its test and traps; a build
whose optional goal fails reports success with no proof diagnostic; the receipt of a program with
and without optional goals lists the same required goals.
