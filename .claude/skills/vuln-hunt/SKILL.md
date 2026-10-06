---
name: vuln-hunt
description: Systematically hunt security-relevant bugs and vulnerabilities in witchy against its specific threat model — twin-backend parity divergence, compiled-WASM memory safety, capability bypass, sandbox escape, and coven supply-chain. Use when asked to vuln-hunt, security-review the compiler/runtime/stdlib/coven, find variants of a known bug, or audit a high-risk subsystem. Combines variant analysis (GRAPE, WOOT'26) with ultra-granular context building, tailored to witchy.
---

# vuln-hunt

Hunt vulnerabilities in witchy **the way its threat model actually breaks**, not
generically. Its executable contract is three-routed: an independent expected
result, the canonical SafeWIR interpreter, and admitted Wasm. The source tree
walker is frozen, not a semantic oracle or RFC-0167 acceptance evidence, and is
retained temporarily only for compiler services. The highest-value bugs live in
disagreement among those three routes and in the hand-written WASM-heap runtime.

This skill fuses three things:
1. **witchy's threat model** (below) — where to look.
2. **Variant analysis** (GRAPE, *Squeezing Juicy Variant Bugs Out of Modern
   Browsers*, WOOT'26) — once you have one bug, ~40% of future 0-days are
   *variants* of it; hunt them systematically.
3. **Ultra-granular context building** (Trail of Bits) — slow, line-by-line,
   first-principles reading beats gist-level guessing.

`$ARGUMENTS` may name a target: a subsystem (`dict`, `capabilities`, `coven`), a
file, a diff/PR, or a known bug to find variants of. With no argument, sweep the
highest-risk surfaces in priority order (below).

---

## Rationalizations — DO NOT skip (read this every run)

| Rationalization | Why it's wrong | Required action |
|---|---|---|
| "Both executable forms print the same, so it's fine" | Agreement alone can preserve the same bug. | Start with an **independent expected result**, then compare canonical SafeWIR and admitted Wasm. |
| "This helper is simple" | The dict-corruption bug (W-001) was one wrong term in a heap-pointer expression. | Read every `ensure()`, every `heap =`, every offset, by hand. |
| "It type-checks / the suite is green" | The suite is green *for the patterns it covers*. W-001 slipped through because no test did remove→reinsert→iterate. | Hunt the **untested pattern**, then add the test. |
| "The canonical interpreter is correct, so the program is correct" | Canonical SafeWIR must agree with an independent expected result, and admitted Wasm is what ships. | Always establish the independent result, run canonical SafeWIR, and run admitted Wasm. |
| "I found and fixed the bug, done" | ~40% of 0-days are *variants* of a known bug. | Do the **variant sweep** (Phase 4) before closing. |
| "This is taking too long" | Rushed context → hallucinated or shallow findings. | Slow is fast. |

---

## witchy threat model (where the bugs are)

Ranked by severity-of-class. Hunt high to low.

1. **Canonical-conformance divergence (PRIME).** Any observable behavior must
   match the independent expected result, canonical SafeWIR, and admitted Wasm,
   or loudly error consistently. A silent admitted-Wasm divergence is the
   crown-jewel bug; a canonical mismatch with the independent result is also a
   bug. Use three-way execution, including errors.
   - **Gotcha:** `witchy <file>` and `witchy sandbox` exercise admitted Wasm;
     they do not establish an independent expected result. Use canonical
     SafeWIR execution helpers and an explicit expected value in the test.

2. **Compiled-WASM memory safety.** The admission kernels
   (`crates/witchy-wir/src/canonical_wasmir/` — the allocator, dictionary,
   drop-worklist and closure-owner kernels) do **manual heap management** over
   layouts the SafeWIR checker (`crates/witchy-wir/src/canonical_safewir/check/`)
   must have proved. Bug classes:
   - **Missing / under-sized `ensure()`** before a heap write → OOB store/trap
     (W-000a: `int_to_string` dropped its `ensure()` in the WAT→WIR port).
   - **Heap not advanced past the full allocation** → the unreserved tail gets
     stomped by the next allocation (W-001). Smell: `ensure(BIG)` but
     `heap = …(SMALL)` where SMALL < BIG.
   - OOB **load/store offsets**, off-by-one on entry strides, sign-extension of
     i32 lengths, integer-overflow in a size computation feeding `ensure`.
   - The **in-place / own-ABI capacity model** (RFC-0016): an op produces a
     structure whose tracked capacity exceeds its reserved heap, so a later
     in-place append writes past it (W-001).

3. **Capability bypass / sealing.** Capabilities are explicit values threaded
   from `main`; there's no ambient authority. Bug classes:
   - A **narrowed/attenuated** capability that still permits a dropped right
     (e.g. `cap as ReadOnly` that can still write) — check `capabilities.rs`,
     `grants`, the narrowing/`as` typeck.
   - **Sealing escape** (RFC-0002): constructing or destructuring a `capability`
     (sealed type) outside its declaring module (`check_sealing` in the linker).
   - A capability **reachable without being passed** (e.g. a host intrinsic
     callable without the cap value) — canonical SafeWIR and admitted Wasm.
   - Footprint **under-reporting**: `witchy caps` claims fewer rights than the
     program can actually exercise (the footprint analyzer in `pm`/`compiler`).

4. **Sandbox escape** (`witchy sandbox`, deny-by-omission; enforcement in
   `crates/witchy-runtime/src/confine.rs` + `crates/witchy-confinement/`).
   A program reaching a capability/host effect that wasn't granted; a net policy
   that lets through a host:port it shouldn't; the WASM sandbox failing to
   confine an effect the footprint claimed was absent.

5. **Supply chain (coven / pm).** The registry signs records and the package
   manager computes capability footprints + gates widening. Bug classes:
   - **Signature / trust bypass** — accepting an unsigned or wrong-key record;
     TUF/trusted-publishing gaps.
   - **caps-diff evasion** — a package that widens its capability footprint
     without the diff flagging it (e.g. a capability reachable through a path the
     footprint analyzer misses) → the supply-chain crown jewel.
   - **Promotion / publish identity confusion** (see project memory).

6. **Comptime / derive / linker.** Compile-time code runs with zero capabilities
   (`emit` = the only channel) — verify it can't escape that. The derive/comptime
   expansion and the std-pull-in have had ordering bugs (`project_std_derive_link_gap`).

---

## Method

### Phase 0 — Orient
- Pin the target: a subsystem, a file, a diff, or a known bug (variant mode).
- State which threat-model class(es) apply. Read the relevant project memories
  (`project_compiled_dict_reinsert_bug`, `project_std_derive_link_gap`,
  `project_cmp_helpers_not_redundant`, `project_codegen_gaps`, the capability
  ones) so you don't re-derive known ground or re-find fixed bugs.
- Open `security-eval/VULNS.md` (the gitignored log) — scan past findings;
  their **variant-analysis** notes are leads.

### Phase 1 — Build context (ToB-style, ultra-granular)
- Read the target **line by line**. For each function/helper: purpose, inputs &
  **assumptions** (incl. implicit: heap state, capability held, which backend),
  outputs & effects, and the **invariant** it must preserve.
- Write the invariants down explicitly (don't trust memory). For heap helpers:
  what does `ensure` reserve, where does `heap` end up, what's the entry layout.
- Propagate through the **call chain** — a helper's assumption becomes its
  caller's obligation. Treat any cross-backend or host-boundary edge as hostile.

### Phase 2 — Hunt (apply the lenses)
- For each threat-model class in scope, ask the bug-class questions above.
- **Differential probing** is the workhorse for parity + memory safety: write a
  small program exercising an **untested edge** (boundary sizes, empty/one/many,
  remove→reinsert, alias-then-mutate, near-page-boundary, recursion depth,
  unicode/multibyte, neg div/mod, NaN/-0.0/inf, big-int), run it on **both**
  backends, and diff. A divergence — or a compiled-side trap/garbage — is a hit.
- Use the GRAPE **bug model** to characterize any candidate (next section).

### Phase 3 — Verify (don't ship a guess)
- **Minimal repro**, deterministic, with the independent expected result,
  canonical SafeWIR result, and admitted-Wasm result stated explicitly.
- Confirm it's real, not a measurement artifact: re-run, shrink, vary the type.
- Rate severity by class (see VULNS.md). For a *parity* bug, identify which side
  is wrong. For a *memory* bug, show the OOB/garbage/trap.

### Phase 4 — Record + variant sweep (the highest-leverage step)
- Append an entry to `security-eval/VULNS.md` (template at the top of that file).
- If fixing: surgical fix → re-run the repro → **full canonical-conformance
  suite** (independent expectation, SafeWIR, and admitted Wasm; no
  regression) → un-ignore/add the regression test.
- **VARIANT SWEEP (do not skip):** the root cause is a *pattern*; find its other
  instances using GRAPE's four properties:
  - **Code clone** — the same code copy-pasted elsewhere (grep the expression).
  - **Similar caller** — sibling functions/helpers with the same shape (e.g.
    *every* WIR helper doing `ensure(BIG)` then `heap = …`; W-001's sweep checked
    all dict + list helpers).
  - **Same callee** — other callers of the same primitive that share the unsafe
    assumption (e.g. everything that calls `ensure`/`dict_index_put`).
  - **Same callee chain** — the root cause spans several functions/offsets; check
    other paths that reach the same state.
  - Record what you swept and what you ruled out (so "bug-class contained" is
    evidenced, à la W-001).

---

## GRAPE bug model (characterize + find variants)

From WOOT'26. Annotate any finding with four elements, then hunt variants.

- **Context** — the function/block/offset where the unsafe call lives.
- **Assumption** — the runtime fact expected there (e.g. "`heap` is past every
  allocated entry slot"; "this capability has been narrowed to read-only";
  "object X is alive").
- **Violation** — a condition that contradicts the assumption (e.g. "`heap`
  points inside an allocated slot"; "the write right survived narrowing").
- **Abuse** — the operation that turns the violation into impact (the next
  allocation stomping the slot; the write executing; deref of a freed object).

A *variant* is the same (assumption, violation, abuse) reached by a different
context. The four properties above (clone / similar caller / same callee / same
callee chain) are how you enumerate the other contexts.

---

## witchy-specific high-value targets (no-argument sweep order)

1. **`crates/witchy-wir/src/canonical_wasmir/` kernels + the SafeWIR checker** —
   every kernel that allocates, grows, or writes a header, and every checker
   rule that admits an extent. Check: reserve-past-full-allocation, every
   offset/stride, size overflow into allocation, a checker rule that accepts a
   fact the producer did not prove.
2. **`crates/witchy-lower/src/analysis.rs` (uniqueness/own-ABI) + codegen
   in-place paths** — capacity vs reserved heap; alias-then-mutate value-semantics
   (the `inplace_*_alias_safe` tests are the spec).
3. **Parity differential sweep** — pick stdlib ops and language features and diff
   interpreter vs compiled on edge inputs. This is how W-001 surfaced.
4. **`crates/witchy-caps/src/capabilities.rs` + sealing/narrowing in typeck +
   `crates/witchy-runtime/src/confine.rs`** — capability bypass, sealing escape,
   footprint truth.
5. **The compiler's input boundary** (`src/source/package_inputs*`,
   `src/source/precompiled_library.rs`) — what explicit `--dep`, `--dep-owner`
   and `--precompiled` selections can claim, and that a library's bundle binds
   its owner. Registry verification itself lives in `../grimoire`/`../coven`.

## Output

- A short report: target, what was checked, findings (each with the GRAPE
  annotation + severity + repro), and the variant sweep.
- For each real finding: a `security-eval/VULNS.md` entry.
- Never claim "secure" — claim "checked X classes against Y surface; found Z".
  Note explicitly what you did NOT cover.
