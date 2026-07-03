---
rfc: 0051
title: Memory safety by construction — rc invariants, one allocator, and deleting the per-method zoo
status: proposed
created: 2026-07-03
predecessors:
  - "0035 (completing the RC floor — this RFC carries forward its 'remaining' section and the SEC-037 guard's rigorous follow-up)"
  - "0016 (reference-counted memory — the reclamation design whose thesis this closes out)"
  - "0023 (checked heap — the ensure()/redzone discipline this makes structural)"
tracking:
---

# RFC-0051: Memory safety by construction — rc invariants, one allocator, and deleting the per-method zoo

## Summary

The compiled backend's memory safety currently rests on three **conventions**
— a runtime plausibility heuristic in `$rc_dup`, a by-construction-but-unenforced
dup/drop lockstep, and a "remember to call `ensure()`" rule for every hand-written
allocator — plus a per-method family of in-place helpers (`*_cap` + `self_*`)
that CLAUDE.md has long declared technical debt. This RFC replaces the three
conventions with three **invariants**:

- **I1 — typed dup/drop emission.** Codegen never emits `$rc_dup`/`$rc_drop` on a
  value whose static type is not an owning object reference. The SEC-037 runtime
  plausibility check becomes dead code and is deleted (after one release as a
  fire-and-report debug assertion). `$rc_drop` gets the symmetric guard in the
  interim.
- **I2 — one allocator.** A single WIR allocation construct is the only thing
  allowed to advance `$heap`, always `ensure()`-prefixed; a workspace test walks
  the assembled WIR and fails on any other `$heap` write.
- **I3 — delete the zoo.** With the RC floor now default-on (the precondition the
  2026-06-30 investigation found missing), the six `*_cap` helpers and the
  `self_*` recognizer family are re-keyed onto the general ownership-driven
  in-place path and deleted, with `check.sh` green and the kernel-clock benchmark
  suite inside an agreed regression threshold as the acceptance gate.

This partially supersedes the follow-up items recorded in
[RFC-0035](0035-completing-the-rc-floor.md) (which is `implemented` and frozen;
its dev-log's "remaining" section and the SEC-037 finding's "rigorous follow-up"
note are the work items this RFC formalizes). It does not reopen RFC-0035's
shipped mechanism.

## Motivation

All residual memory-safety risk on the compiled backend is Layer C (intra-guest
corruption, contained by wasmtime + host-side capability checks — no host escape
path is known). But witchy's own bar is higher than "contained": the parity
doctrine is *loud or identical*, and heap corruption is the canonical source of
silently-wrong answers. Three specific conventions are carrying that weight
today, and each has already produced a shipped bug.

### 1. The `$rc_dup` guard is a heuristic, and it ships default-on

SEC-037 (see `security-eval/findings/SEC-037-*.md`) was a real use-after-free:
`$rc_dup` did `[ptr-8]++` on pointers that were not `$rc_alloc` object bases —
views/slices into a parent object, or mis-typed scalars above `heap_base` — and
the increment landed on the *parent's data*, corrupting a length word and
producing OOB reads in minigrep and the pm suite. The fix (commit `974ccee`) is
the 2-factor plausibility guard in
`crates/witchy-wir/src/wir_helpers/mod.rs:508-560`: the increment runs only when
the size word `[ptr-4] & 0xFFFFFF` is in `[1, 2^20)` **and** the rc word
`[ptr-8]` is in `[1, 2^24)`, nested under `ptr >= heap_base` (nested, not
`And`-ed, because WIR `And` evaluates both operands and the header loads would
trap on small scalars).

The guard is *directionally* sound: a genuine object always passes both checks,
so no dup is ever lost — a skip can only leak, never free live data. But it is
**probabilistic against the other direction**: a non-object pointer whose two
header words are *coincidentally* plausible still gets its neighbor's data
incremented. The guard's own comment calls this "vanishingly unlikely"; the
honest characterization is *a plausibility heuristic where an invariant should
be*. And since `974ccee` promoted `rc-floor`, `release == all`
(`crates/witchy-syntax/src/opt.rs` — `default_on` returns `true` for every
lever), so this heuristic is what every user runs by default.

### 2. `$rc_drop` has no guard at all; soundness rests on unenforced lockstep

`$rc_drop` (`wir_helpers/mod.rs:570`) checks only `ptr >= heap_base`, then frees
at `rc <= 1`. Its soundness is the **dup/drop lockstep**: a drop may only ever
release a reference a dup created. Today that lockstep is real but purely
by-construction:

- The dup is emitted at **one** site: the `list.at` element read
  (`crates/witchy-lower/src/codegen/builtins.rs:823-837`), gated on
  `Kind::I32 && list_elem_is_offset0_rc(receiver) && rc-floor`.
- Drops are emitted at **three** sites in `crates/witchy-lower/src/codegen/mod.rs`:
  the displaced element at an in-place `set_at` (~:2669-2698, gated
  `inplace_push` + `expr_is_offset0_rc` + `wm_level == 0`); read-owned bindings
  at last use (~:3159-3181, gated on `rc_owned_bindings` — populated at
  ~:2172-2189 under *the same predicate as the dup*, so drop-iff-dup holds by
  construction); and the match-on-read scrutinee after the arms (~:3495-3594,
  same `list.at` + `list_elem_is_offset0_rc` gate, bounded by `SCRUT_POOL`).

Every one of those gates is a hand-maintained mirror of another gate in another
file. If a future edit widens a drop gate without widening the dup gate, a
never-dup'd value gets decremented — count underflow — and `$rc_drop` frees a
live object. Nothing in the test suite would catch the *mechanism*; only a
corpus program that happens to hit the widened case would.

### 3. `ensure()` is a convention, and the class is demonstrably open

Every hand-written WIR helper that bumps `$heap` must remember to call
`$ensure` first. Two shipped bugs prove the class is open, not theoretical:

- The `int_to_string` OOB (the coven publish corruption): the WIR port dropped
  the `ensure()` and wrote digits past the last page. Fixed — it now allocates
  through `$rc_alloc` (`wir_helpers/mod.rs:807-816`).
- July 2: `$list_push_cap` took its in-place branch on an analysis claim
  (`cap > len`) with **no bounds check** against the real allocation, so a
  false-positive uniqueness answer meant a silent OOB store (commit `552180f`,
  fixed by trapping against the `[ptr-4]` size header).

Today `$heap` is advanced raw at four sites besides `$rc_alloc`
(`wir_helpers/mod.rs:387`): the dict index rebuild
(`wir_helpers/dict.rs:508`), the worker-VM `__galloc`
(`wir_helpers/vm.rs:35`), the string-export wrapper's `__galloc` twin
(`codegen/assembly.rs:760`), and the watermark **rewind**
(`codegen/mod.rs:3886` — a reset, not an allocation, but still a raw write to
the same global). Each is individually correct *now*; the next one is one
forgotten `ensure()` away from the `int_to_string` bug again.

### 4. The per-method zoo persists, and its stated deletion precondition is now met

CLAUDE.md's central optimization rule says the in-place machinery must be one
general mechanism, names the `*_cap` + `self_*` family "technical debt to
delete," and defines the proof: *removing it with the suite still green*.
Reality: six `*_cap` helpers survive (`list_push_cap` `wir_helpers/mod.rs:1228`,
`list_set_cap` :1376, `list_update_cap` :1440, `str_append_cap` :1516,
`dict_insert_cap` `dict.rs:383`, `dict_update_cap` `dict.rs:583`), fed by the
recognizer family in `crates/witchy-lower/src/analysis.rs:88-256`
(`self_push_elem`, `self_insert_args`, `self_update_args`, `self_set_at`,
`self_update_at`, `self_concat_pieces`, `self_record_update`, plus
`self_own_call` and the unifying `InPlaceOp`/`self_inplace_op` dispatcher). The
June-30 `InPlaceOp` unification tidied *dispatch* but deleted nothing — and a
new per-method case landed the same day (`f4a0415`, `RecordUpdate`).

The recorded reason the zoo survived the 2026-06-30 dedup run: thin-builtins /
own-ABI could not subsume the *mutating* ops without a perf-negative RC floor —
without a refcount, no general mechanism could know a buffer was safe to reuse,
so the static `cap` token had to be threaded per-method. **That precondition has
since flipped.** The RC floor shipped default-on (`974ccee`): every heap object
now carries `[rc][size]` at `ptr-8`/`ptr-4`. The stated blocker is gone; either
the zoo is now deletable, or the 2026-06-30 conclusion holds for a *new* reason
that must be named and recorded (see Drawbacks — both outcomes are acceptable;
the current state, where the rule and the code contradict each other silently,
is not).

## Design

### I1 — typed dup/drop emission (the guard becomes dead code)

**Invariant:** codegen never emits `$rc_dup` or `$rc_drop` on a value whose
static type is not an owning, offset-0 `$rc_alloc` object reference. Views,
slices, scalars, Dict-interior pointers, and bare type-vars are excluded **by
type**, at the emission site — not by a runtime plausibility test.

This is less new code than it sounds: the emission gates already *are* type
predicates (`list_elem_is_offset0_rc` / `expr_is_offset0_rc` in
`codegen/types.rs:141/:180`). What changes is their **status**: they go from
"first line of defense, with the runtime heuristic as the backstop" to "the sole
and sufficient gate." Concretely:

1. **Close the SEC-037 gap at its source.** SEC-037's mis-dup'd pointer was a
   view/slice reaching the dup site because the *type* predicates answered on
   the container expression, not on what the read actually produces under the
   `views` lever. The predicates must answer over the post-optimization
   representation (a view-producing read is not an owning reference), which
   means the view/SROA/packed candidacy sets feed the predicate — they are all
   already in `Codegen` state.
2. **Symmetric interim guard on `$rc_drop`.** Until (1) is proven, add the same
   2-factor plausibility check to `$rc_drop` that `$rc_dup` has. Direction of
   error is the safe one: a skipped drop leaks; it never frees live data. This
   is a ~10-line WIR change and should land immediately, independent of the
   rest of this RFC.
3. **Demote the heuristic to a debug assertion, then delete it.** Once I1
   holds, the plausibility check in `$rc_dup`/`$rc_drop` is dead code. For one
   release, keep it in sanitizer builds only (folded into `WITCHY_HEAP_CHECK`,
   which already instruments allocation) with the polarity **inverted**: an
   implausible header at a dup/drop site *traps and reports* (via the
   `WITCHY_WASM_BACKTRACE` name-section machinery — cf. RFC-0045 on compiled
   trap diagnostics) instead of silently skipping. A silent skip hides an I1
   violation; a trap names the emission site that broke the invariant. After a
   release with zero fires across the fuzzer + examples + e2e, delete it.

**Verification (the test is the invariant):**

- **Independent dup/drop toggles.** Two test-only knobs disable dup emission and
  drop emission separately. The lockstep property becomes assertable instead of
  by-construction: with dups disabled, a `__rc_drops_fired` counter must read
  zero over the corpus (drops key off recorded dup facts, so they must stand
  down together); with drops disabled, every corpus program's output must be
  byte-identical (leak-only). Any drift between a dup gate and its drop mirror
  now fails a test instead of corrupting a heap.
- **The UAF sanitizer sweep** (`check.sh`'s `uaf_sanitizer` differential-fuzz
  job — poison-fill + no-reuse) runs against the corpus with the heuristic
  deleted; it is the regression net for exactly the SEC-036/SEC-037 class.
- The existing `rc_corpus_*` matrix and `examples_agree_under_rc_floor`
  metamorphic guard (RFC-0035's gate) stand unchanged.

### I2 — one allocator (the `ensure()` class closes structurally)

**Invariant:** exactly one construct advances `$heap`, and it is
`ensure()`-prefixed by construction. Everything else calls it.

- `$rc_alloc` is already the front door for every value-producing allocation
  (RFC-0035 made it so) and already ensures before bumping. Promote it to *the*
  allocator: route the dict index rebuild (`dict.rs:508`), `__galloc`
  (`vm.rs:35`), and the export wrapper (`assembly.rs:760`) through it — or,
  where the `[rc][size]` header is genuinely unwanted (the worker VM's
  whole-heap-drop model needs no headers), through a shared `$bump_alloc` core
  that `$rc_alloc` itself calls, so the ensure+bump pair exists in exactly one
  place. The watermark rewind (`codegen/mod.rs:3886`) is a *rewind*, not an
  allocation; it is exempted by name.
- **The enforcement test is structural, not grep.** A workspace test assembles
  the full WIR module (every helper + a representative lowered program) and
  walks every function body: any `SetGlobal { global: "heap" }` outside the
  single allocator (and the named rewind sites) fails the test with the
  offending function's name. Because all WIR construction funnels through
  `assemble_wir_module`, the walk sees everything, including future helpers —
  the test cannot be forgotten the way `ensure()` can.
- The `552180f` lesson generalizes here too: any in-place store derived from an
  analysis *claim* must be bounds-checked against the `[ptr-4]` size header
  (trap, not trust). With I2, that check lives in one place instead of six.

### I3 — delete the zoo (the general path absorbs the six operations)

**Invariant (CLAUDE.md's, now with its precondition met):** one general
mechanism, driven by the ownership conventions and the escape/uniqueness
analysis, serves every in-place operation. No per-method helpers, no
per-method recognizers.

Mechanism, in two rungs:

1. **One parametric in-place protocol replaces six helpers.** Every `*_cap`
   helper is the same algorithm with different constants: *check the ownership
   token; if in-place is legal, bounds-check the write against `[ptr-4]` and
   store/append at a type-derived offset; else copy-and-grow through
   `$rc_alloc`, dropping the displaced value via `$rc_drop`.* The element
   width, header layout, and grow policy are functions of the receiver's static
   type — exactly the information `InPlaceOp` dispatch already has in hand.
   Replace the six hand-written helpers with one WIR-level protocol
   parameterized by an operation descriptor, and replace the seven `self_*`
   shape matchers with a single rule: *a self-assign whose RHS is a builtin
   (or own-ABI callee) that consumes its receiver at argument 0*, keyed off a
   signature table instead of per-method AST matchers. `self_own_call` already
   works this way; the builtins join it.
2. **The end-state rung (Perceus reuse): the runtime rc subsumes the static
   token.** With the `[rc]` word on every object, `rc == 1` *is* the uniqueness
   fact, computed exactly, at runtime — the same resolution RFC-0035 used to
   dissolve the executor's inter-procedural aliasing question. A
   value-semantics builtin can branch on it directly: `rc == 1` → mutate in
   place; `rc > 1` → copy. That deletes even the recognizer rule of rung 1.
   **Gated hard**: this rung is sound only when dup coverage is *total* (a
   missed dup makes `rc == 1` a lie, and an in-place mutation observable
   through an alias is a silently-wrong answer — worse than a leak). It ships
   only after I1's typed emission is proven at every aliasing point, and it is
   a separate, opt-in lever until the invariance sweep says otherwise. Rung 1
   does not wait for it.

**Acceptance (the deletion is the proof):**

- The six `*_cap` helpers and the per-method `self_*` matchers are **deleted** —
  not bypassed — with `./scripts/check.sh` green.
- The kernel-clock benchmark suite (`benchmarks/` — `nsieve`, `fannkuch`,
  `expr_eval`, `binary_trees`, `dict_count`, `list_sum`, `loop_sum`,
  `word_count`, `record_build`, `knucleotide`) shows **no per-benchmark kernel
  regression beyond 5%, and no geomean regression beyond 2%**, measured by
  `benchmarks/run.sh` against the pre-deletion commit. These are the shapes the
  zoo exists to serve; if the general path can't hold them, the deletion
  doesn't ship.
- **Fallback position:** if a specific operation cannot meet the perf bar, it
  is retained *per-op*, with a comment linking the measurement that justifies
  it — a documented exception, not silent drift. The **default is deletion**;
  retention requires the number.

### The header canary (weighed, not chosen)

A magic byte in every object header, checked at dup/drop/free, is the
belt-and-suspenders alternative to I1. Weighed: the infrastructure half-exists —
the high 8 bits of the `[ptr-4]` size word are already reserved and used by the
`WITCHY_TYPE_CHECK` sanitizer as a type tag — but a canary is still
probabilistic (a coincidental match is rarer than SEC-037's two-word
coincidence, not impossible), costs a load+compare on every count op in
release, and shares the reserved byte with the type tag. I1 is static, free at
runtime, and categorical. **Recommendation: I1 primary; a canary only as an
optional debug check under `WITCHY_HEAP_CHECK`**, where it composes with the
existing redzone sweep and costs nothing in release.

### Relationship to RFC-0005 (externref)

Complementary, different surface. RFC-0005 removes *forgeable capability
handles* at the host boundary (security: a guest corruption must never widen
authority); this RFC removes *heuristic reclamation guards* inside the guest
heap (safety: a codegen bug must never corrupt data). They meet in the middle:
once cap-carrying values move to GC structs (RFC-0005 stage 4), they leave the
linear heap entirely, shrinking the domain `$rc_dup`/`$rc_drop` can even
observe — I1's type predicate and RFC-0005's `carries_cap` classification are
two projections of the same static-type discipline. Neither blocks the other.

## Alternatives

- **Keep the heuristic, harden the constants.** Widen the plausibility window
  checks, add a third factor. Rejected: it converges on a canary with extra
  steps and stays probabilistic; the whole point is to stop betting on header
  coincidences.
- **Canary as primary** (instead of I1). Rejected above — runtime cost in
  release, still probabilistic, and it *masks* emission-site bugs that I1's
  debug assertion would name.
- **Full-program conservative RC** (dup/drop everything, no static gates).
  Sound and simple, but re-litigates RFC-0016's cost model — the elision ladder
  exists precisely to avoid universal count traffic; regression on the tight
  scalar loops that are witchy's remaining perf gap is near-certain.
- **Do nothing.** The suite is green today and the guard has an honest
  soundness direction. Rejected: `release == all` means the heuristic is the
  default user experience; the drop side is unguarded; the `ensure()` class has
  shipped two real bugs; and the CLAUDE.md rule is in open contradiction with
  the code, which corrodes the rule.

## Drawbacks

- **I3's perf risk is real, and the 2026-06-30 conclusion may still hold.** The
  finding that the arms are principled was empirical; the RC floor's arrival
  changes the inputs, not the verdict. If the general path (rung 1) measurably
  loses to the hand-written helpers on the benchmark gate — plausible for
  `str_append_cap`'s hot concat spine — then I3 is **formally rejected-in-part**:
  the surviving ops get their measurement-linked comments, this RFC's status
  records the partial rejection, and CLAUDE.md is amended to say the arms are
  load-bearing and why. That is a *valid outcome* of this RFC — the deliverable
  is a decided, evidenced position either way, replacing the current
  contradiction.
- **I1 leans on the type predicates being right**, including under the `views`/
  SROA/packed levers — the exact interaction that produced SEC-037. The debug
  assertion release is the mitigation, but a wrong predicate before the
  assertion lands is the same UAF class as today. Sequencing matters: the
  `$rc_drop` interim guard and the toggle tests land *first*.
- **I2 touches the hottest code in the backend.** The bump path runs on every
  allocation; routing `__galloc` and the dict rebuild through a shared core adds
  a call frame unless the WIR inliner handles it. Mitigation: `$rc_alloc` is
  already the dominant path (RFC-0035 routed the value producers through it),
  so the change is to the cold stragglers; measure with the same benchmark gate.
- **More test machinery** (toggles, counters, the WIR walk) is more surface to
  maintain — accepted, because each piece converts an unenforced convention
  into a failing test.

## Prior art

- **Perceus / Koka FBIP** (Reinking, Xie, de Moura, Leijen) — precise RC with
  reuse; the `rc == 1` in-place branch of I3 rung 2 is Perceus's
  reuse-analysis made dynamic; RFC-0016/0035 already adopt its dup/drop
  discipline and ⊥-keeps-the-count soundness floor.
- **Lean 4's runtime** — the same reuse-token idea (`reset`/`reuse`) shipped in
  a production compiler; evidence the static-token → runtime-rc migration is
  viable.
- **ASan redzones / heap canaries** — the shape of the rejected-as-primary
  alternative; witchy's `WITCHY_HEAP_CHECK` redzone sweep (RFC-0023) is already
  this, correctly scoped to debug builds.
- Internal: RFC-0016 (design), RFC-0035 (the shipped floor + the follow-up this
  formalizes), RFC-0036 (the executor residual — untouched here), RFC-0023
  (checked heap), RFC-0005 + its implementation plan (externref; the
  complementary boundary), SEC-036/SEC-037 (`security-eval/findings/`).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
