---
rfc: 0058
title: "Differential-testing harness integrity: make the parity gate able to fail"
status: proposed
created: 2026-07-04
related:
  - "0001 (the parity prime directive this harness exists to enforce)"
tracking:
---

# RFC-0058: Differential-testing harness integrity

> Provisional throughout. Code blocks are intentionally **not** tagged `witchy` so the
> doc-examples sweep does not compile pre-implementation snippets.
>
> **Numbering note:** a concurrent effort reserved "0058+" for a consistency-gaps batch.
> This RFC took the next free number (0057 was the prior max); if it collides, renumber —
> it is design-first and referenced nowhere yet.

## Summary

witchy's one inviolable rule is parity: every behavior identical (or loudly-erroring) on both
backends. Two independent evaluations found that the machinery meant to *enforce* that rule
can pass while checking essentially nothing:

- The **example parity sweep** (CI + `just parity-sweep`) discards the `parity` exit status
  (`|| true`) and fails only when the literal string `DIVERGE` appears in stdout — so a
  compile error, a missing file, a crash, or any non-divergence error passes the gate
  (BUG-002).
- The **differential fuzzer** generates programs that trap in their first 0–7 statements
  (unclamped `list.at` indices, `% 1_000_000` ints), and its both-error arm compares zero
  output — so silent value divergence, the class it exists to catch, is invisible (BUG-003).
- Neither has a **vacuity guard** (no "assert we compared > 0 programs / lines") or a
  **positive control** (no seeded divergence proving the gate *can* fail); the metamorphic
  law set is satisfiable by `list.sort = identity`.

Each is a fixable bug. But collectively they are a design gap: **the project has no stated
contract for what makes a differential test able to detect a regression.** This RFC defines
that contract — total generators, fail-closed sweeps, mandatory vacuity guards, per-config
accounting, and positive controls — so that "the parity gate is green" means what it claims.
It does not change the language; it changes how the language's core invariant is verified.

## Motivation

A safety net that cannot fail is worse than none: it manufactures false confidence, and every
fix merged "because parity stayed green" is unverified. The deep-eval fixes that *did* land
(SEC-040…045, the `bytes.slice` parity repair) were caught by hand-written pinned-output
tests and ad-hoc probes — **not** by the fuzzer or the sweep, which is the tell. The
`Mono`-rename wrong-answer bug (BUG-001) is the worst case made concrete: both backends agree
on the wrong result, so a value-blind harness is structurally incapable of seeing it.

## Design — the harness contract

A differential check earns its place in the gate only if it satisfies all of:

### 1. Total generators
A generated program must **run to completion** on both backends barring an *intended*,
classified trap. Indices clamp to `0..len`; divisors are guarded; integers stay in a range
that survives `${...}` rendering. Trapping is a deliberate, tagged case (a minority), not the
default outcome. Target: a measured **median ≥ 1 comparable output line per program**, and a
hard failure if > X% of a run traps before its first observable effect.

### 2. Fail-closed classification
`witchy parity` returns **distinct exit codes**: agree / diverge / intended-trap-agree /
unexpected-error. The sweep and the fuzzer branch on the code, never on a substring; any
`unexpected-error` fails the gate. `|| true` is banned in gate scripts.

### 3. Value comparison across traps
A both-error outcome compares the **pre-trap output prefix and the failure point**, not just
"both errored." Two backends that print different lines and *then* both trap is a DIVERGE.

### 4. Vacuity guards + positive controls
Every sweep asserts a **minimum compared count** (files discovered, programs run, lines
compared) and ships a **seeded-divergence control** that must be detected — a self-test that
the gate can fail. Program discovery is centralized (one script), so a path/layout change
can't silently empty the corpus; non-runnable entries live on an explicit, reviewed skiplist.

### 5. Per-configuration accounting
Skip/agree/diverge is tracked **per optimizer config**, not only for the production default.
A config that *skips* where the default *agrees* is a failure to explain, not silent slack.
Baselines are `default_set().without(lever)` (already partly adopted), never binary-vs-itself.

### 6. Metamorphic laws with teeth
Laws must be falsified by a plausible wrong implementation. `sort` carries a **sortedness +
permutation** law (not just idempotence + length); dicts carry a **remove/reinsert/iterate**
law; encoders carry a **round-trip** law. A law satisfiable by `identity` is not a law.

## Non-goals

- No new language surface, no runtime change — this is test-infrastructure only.
- Not a rewrite of the 415-test pinned-output corpus, which is real and stays; this hardens
  the *generative* and *sweep* layers around it.
- Does not attempt property-based *shrinking* of counterexamples (a possible later addition).

## Rollout

Design-first. Implementation is a sequence of contained, individually-verifiable steps —
each of BUG-002 and BUG-003 is one — landing behind the harness itself (the first commit adds
the vacuity guard + positive control, so every subsequent step is proven to be catchable).
No parity-gate change ships without demonstrating, via the seeded control, that the gate can
still fail.

## References
- `bugs/BUG-002-parity-sweep-swallows-failures.md`, `bugs/BUG-003-differential-fuzzer-traps-early.md`
- `scratch/deep-eval/MERGED-TRIAGE.md` §Tier-2 (both evaluations, cross-verified)

## Review note (2026-07-04)

From the full open-RFC review (scratch/rfc-review-2026-07-04.md, verified against
HEAD 789f2e9) — this review served as the pending design review.

**Status-accuracy corrections.** All six contract points verified real: BUG-002's
`grep -qi "DIVERGE"` + `|| true` classification in BOTH justfile:81-83 and
ci.yml; BUG-003's fuzzer counting both-trap as Agree (the guard is satisfiable by
a corpus of 100%-trapping programs); per-config outcome invisibility (:565-575);
identity-satisfiable sort laws. Two overstatements: partial vacuity guards DO
exist (NLAWS, the grammar-coverage bitmask); and §3 half-exists for routed aborts
via RFC-0045 abort-core matching — the genuinely missing piece (the pre-trap
output prefix) requires API changes to both run harnesses (`Err(String)` carries
no partial lines).

**Required revisions before implementing.** (a) Specify the positive-control
mechanism — a genuinely-divergent fixture can't live in-repo; it needs an
env-gated fault-injection lever, provably inert in release. (b) A
machine-readable parity-stats channel + an exit-code taxonomy
(agree / diverge / both-error-agree / unexpected-error) + a `timed-out` class.
(c) Map the contract onto the existing partial guards. (d) Extend scope:
inventory ALL gate scripts (e2e-full.sh `expect_contains` substring matching,
check.sh `| tail` exit-code masking, validate_book_examples.mjs) and fix the
run_parity temp-file race — fixed file names in a shared temp dir mean the
harness can test the wrong program (BUG-010). (e) A law-growth policy for new
std modules.

**Verdict.** Implement-now after light revision; the guard-first rollout order
is exactly right. Priority: high — cheap, and it gates confidence in everything
after it.

## Revision (2026-07-04)

Coordinator design decisions taken while implementing the contract. These resolve
the five "required revisions" (a)–(e) above and pin the wire formats the gate
scripts and the fuzzer agree on. Status: implemented.

### Positive control — the seeded-divergence lever (a)
`WITCHY_SEEDED_DIVERGENCE=1` is an env-gated fault injector read **only** on the
`witchy parity` code path (`parity_check` in `src/main.rs`). When set, it appends a
control-char-delimited sentinel line (`\u{1}witchy-seeded-divergence\u{1}` — a line no
real program can print) to the *compiled* backend's result before comparison, forcing a
guaranteed `diverge` regardless of the program (an `Ok` gains a line; an `Err` becomes an
`Ok`-with-sentinel, so the empty/both-error and value cases all report DIVERGE). It is
**inert in release**: the program-run path (`witchy <file>` / `sandbox`) never reads it,
so injected divergence is invisible to actual execution. Two tests pin this — a behavioral
one (a normal run is byte-identical with the var set vs unset; `witchy parity` on the same
file returns `agree` unset and `diverge` set) and a static one (the var name appears in
`src/` only, never in the codegen/runtime/interp crates). No divergent fixture files exist
in-repo; the divergence is synthesized at compare time.

### Machine-readable classification + exit-code taxonomy (b)
`witchy parity` prints a final machine-parseable line to stdout:

    parity-stats outcome=<agree|both-error-agree|diverge|unexpected-error> compared=<N> file=<path>

`compared` is the count of output lines actually compared (`0` for both-error-agree and
unexpected-error; the matched-prefix length for a value diverge). Exit codes are distinct
so no consumer greps human text: **0** = agree or both-error-agree (a pass), **2** =
unexpected-error (compile/link/lower failure, missing `main`, missing file — a real
regression for the known-good example corpus; a tolerated generator miss for the fuzzer),
**3** = diverge. The human `✓ … agree` / `✗ … DIVERGE` lines are retained for people and
for the legacy `contains("agree")` assertions, but gates branch on the exit code and the
`outcome=`/`compared=` tokens. **"Intended trap" is not parity's judgment** — parity only
reports the four mechanical outcomes; whether a both-error-agree or an unexpected-error is
acceptable is decided generator-side (the fuzzer tolerates them within its skip budget; the
example sweep treats any non-zero exit as failure, which is the BUG-002 fix).

### Timeout class (c)
The timeout is enforced **harness-side**, around the `witchy parity` child process in the
fuzzer (`run_with_timeout`, `WITCHY_FUZZ_TIMEOUT_SECS`, default 30s), not inside `witchy
parity` itself (a single program-run has no natural wall-clock budget). A child that
exceeds the budget is killed and classified as a distinct `TimedOut` outcome — never
silently counted as agree — and shrink probes inherit a smaller budget.

### Total generators + vacuity guards mapped onto the existing partials (d, §1)
`gen_int` is made total: the `list.at` arm now indexes a freshly-generated list literal of
known length `n` with an index in `0..n` (in-bounds by construction), and the `/`/`%` arms
draw their divisor from `gen_pos_int` — a strictly-positive, small, non-overflowing
expression (`1 + below(1000)`, `1 + list.length(...)`, `1 + string.length(...)`), which is
never `0` and never `-1`, so it dodges both div-by-zero and the `INT_MIN / -1` trap.
(`string.substring` already clamps on both backends, so it was already total.) The two
pre-existing partial guards are **kept and extended, not replaced**: the grammar-coverage
bitmask (every statement kind must appear) and `NLAWS` (every law line must be observed)
stay; added on top is an **execution-volume guard** — the median compared-line count across
the run must be `≥ 1`, so a corpus that traps before its first observable effect now fails
loudly rather than passing vacuously.

### Per-configuration accounting (§5)
The fuzzer accumulates a full outcome matrix **per optimizer config** (agree /
both-error-agree / skip / diverge / timeout / summed compared-lines), not just agree/skip
for the empty default. Cross-config consistency is asserted: if the default config produced
compared output for a seed, no lever config may `skip` or `timeout` it (a lever changing
compilability is a failure to explain). The median-compared and grammar guards run against
the default config's tallies.

### Temp-file uniquification (BUG-010, d)
All harness temp files route through one `unique_temp_path(prefix)` helper: `pid +
monotonic atomic counter + nanos`, so every invocation and every probe — including each
shrink probe (previously the constant tag `"shrink"`) — gets a distinct path. Concurrent
fuzz jobs and a dev run beside a fuzz job can no longer cross-talk.

### Gate-script inventory + fixes (BUG-002, d, §2/§4)
Fixed: the `|| true` + `grep -qi DIVERGE` classification in **both** `justfile`
(`parity-sweep`) and `.github/workflows/ci.yml` (parity job) — now exit-code + stats-line
classified, fail-closed on any non-zero exit, with a `compared > 0` **vacuity guard** and a
**seeded-divergence positive-control step** (run one example under
`WITCHY_SEEDED_DIVERGENCE=1`, assert it diverges). `scripts/e2e-full.sh`'s
`expect_contains "agree"` substring check is replaced with an exit-code + `outcome=agree`
assertion. `scripts/validate_book_examples.mjs` gains a vacuity guard (fail if zero runnable
blocks were validated). The `check.sh | tail` exit-mask is a *usage* footgun (a caller
piping `check.sh` to `tail` drops its status under a non-pipefail shell), documented as a
banned invocation; `check.sh` itself is `set -euo pipefail` and unaffected. Swept and clean:
`pg_validate.mjs`, `local-registry-demo.sh`, `run-witchy-coven.sh` (their `|| true` uses are
on process teardown / log-poll, not on classification).

### Metamorphic laws with teeth + law-growth policy (§6, e)
`gen_law_program` gains, on top of the retained idempotence + length laws: a **sortedness**
law (`is_sorted(list.sort(xs))`) and a **permutation-by-element-counts** law
(`is_perm(list.sort(xs), xs)` via per-element occurrence counts) — the pair is *not*
satisfiable by `list.sort = identity` (identity fails sortedness on any unsorted input),
which the old idempotence+length pair was; and a **dict round-trip** law
(`remove → reinsert → get` returns the value and `pairs`/`length` stay consistent). `NLAWS`
is bumped to match. **Law-growth policy:** every new std module that ships an algebraic or
structural invariant (a reversible transform, a container round-trip, an ordering) must add
at least one law to `gen_law_program` that a plausible *wrong* implementation would falsify
— identity-satisfiable "laws" do not count. New reversible encoders carry a round-trip law;
new ordered containers carry a sortedness+permutation law; new maps carry a
remove/reinsert/iterate law.
