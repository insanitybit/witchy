# RFC-0110 completion — hand-execution plan

Authoritative step-by-step plan to take RFC-0110 from `accepted` to
`implemented`, produced by a understand+design+synthesize agent workflow over
the ownership/uniqueness substrate. Steps are ordered smallest-safe-increment
first, each independently gate-green and ideally PROVEN-able against a ledger
row, with explicit STOP-if-uncertain boundaries around the parity-critical
ownership model.

**Status: Step 1 (counter globals) LANDED. Steps 3-12 remain** — a multi-session
continuation; a fresh context should execute them in order, running the required
full opt-mode blast-radius sweep after Steps 3 and 7.

Verification complete. All load-bearing sites confirmed on current `master` (findings were gathered on the `rfc0110-repair` worktree; structure holds, line numbers re-verified fresh above). Key reconciliation across the three candidate designs, with my tech-lead decisions baked in, follows.

---

# RFC-0110 completion — authoritative hand-execution plan

## Design decisions (why this plan differs from any single candidate)

- **Repair mechanism = Design 1 (minimal), NOT Design 2/3 (eager boundary `rc_alloc`+`MemoryCopy`).** The compiled normal-mode repair *already exists physically*: an untracked/unproven arg is passed `W::ConstI32(0)` today (`mod.rs:2509,4340`), and the `*_cap` helper copies-on-write on first mutation — value-equal to the interpreter's `Rc::make_mut` (`interpreter.rs:1464,1488,1521,1543`). So the "repair" is a **detector-widening + a boundary counter over an already-value-correct path**, with **zero interpreter functional change**. Design 2/3's eager copy adds a new copy path on both backends, needs an interpreter deep-clone mirror that is value-neutral (pure risk), and risks double-copy. Rejected.
- **Repair set MUST be keyed by `(function, source-line, callee, arg-index)`, NOT `&Stmt` pointer.** `try_module_no_copy_misses` runs on `witchy_types::traits::lower(module.clone())` (`analysis.rs:4527`) — the clone destroys pointer identity vs. the codegen module. Design 2/3's "keep `stmt_key`" is unsound for a codegen consumer. Confirmed.
- **The boundary counter must be driven by the lever-independent repair set, NOT by the `inplace_push`-arm choice.** `inplace_push` is gated by the `InPlace` lever, so counting there over-counts under `WITCHY_OPT=none` (criterion 9 requires lever-invariance). Load-bearing.
- **Coverage widens Var+Own only. `let`/`bare unique` is a STOP.** Repair (a re-own copy) is only meaningful for mutation-capable conventions (`Var`→var-cap token, `Own`→own-cap token). `let unique` is `SharedBorrow` (callee cannot mutate → no aliasing hazard). Design 3's "any AccessKind" is wrong.
- **Direct-storage: ship the six-proof gate + Slice A (whole-local write-back streamlining) only. Slice B (callee-mutates-caller pointer ABI) is a hard STOP** — it breaks trap atomicity and is not safe for hand-execution without a proven terminal-VM host-surface audit.
- **Entry-filter drop is isolated into its own high-risk step**, sequenced after coverage widening so blast radii don't compound.

---

## PHASE A — paper-safe (land these first; each is gate-green with no semantic change)

**Step 1 — Declare the three counter globals. Risk: low.**
- Edit: `crates/witchy-lower/src/codegen/assembly.rs` near line 5253 (the `__witchy_indirect_ownership_calls` unconditional-push block). Add three sibling `WirGlobal { kind: I64, mutable: true, export: Some(...) }` pushes: `__witchy_boundary_reown_copies`, `__witchy_ownership_token_repairs`, `__witchy_direct_storage_var_accesses`. Mirror the `__witchy_indirect_ownership_calls` shape at `assembly.rs:5253-5257` exactly (unconditional push, heap-independent scalar).
- Proves: `crates/witchy-runtime/src/runtime.rs:876-889` readers flip `None`→`Some(0)`. `src/stats.rs:129-133` test (`==0`) stays green because `unwrap_or(0)` now reads a real 0.
- Parity: no increment sites, no lowering change, no observable behavior. Interpreter untouched.

**Step 2 — Value-equality anchor for shapes that already compile in normal mode. Risk: low.**
- Edit: `tests/rfc0110.rs`. Add a helper mirroring `compiled_output` (`tests/rfc0110.rs:83`) that also returns `boundary_reown_copies()`. Add one fixture per call shape {direct fn, function-value, lambda, `Apply`, trait method, existential witness, fixed-place field/index} where a `var`/`own` param that *cannot be proven unique* receives an arg — **as it compiles on master today** (normal mode, no `mode opt`). Assert value-equality across interpreter (`run_checked_module`), `OptSet::all`, `OptSet::none`, and every single-lever toggle (reuse the `tests/rfc0110.rs:240-241` lever loop). Do **not** assert a counter yet.
- Proves: normal-mode value-correctness of the zero-token COW path *exists before any change* — this is the parity firewall for the whole program. If any of these diverge today, **STOP** (see STOP-1 below).
- Parity: pure test addition.

---

## PHASE B — normal-mode one-copy repair (criteria 2/8/9; ledger rows 2, 8, 9-repair-half)

**Step 3 — Widen the miss detector to `own unique` (analysis-only, opt-mode stricter). Risk: med.**
- Edit: `crates/witchy-lower/src/analysis.rs:242` `no_copy_var_params` derivation inside `call_ownership_fact` (`analysis.rs:197`). Generalize the filter from `AccessKind::ExclusiveWriteback && Unique|LocalUnique` to *also* include `AccessKind::Consuming && Unique|LocalUnique` (own-unique). Add a distinct `unique_params` accessor on `CallOwnershipFact` (`analysis.rs:146`) if the two token families (var-cap vs own-cap) must be distinguished downstream; otherwise widen the existing vector and rename for honesty. Feed the widened index set to `record_call_misses` (`analysis.rs:4440`) — it already runs for every call shape, so **no per-shape code changes**. Update `no_copy_requirements` (`analysis.rs:3751`) accordingly.
- **EXCLUDE `Let` (SharedBorrow).** DEFER `bare unique` — see STOP-2.
- Proves: extend `ACCESS_DIAGNOSTIC_MATRIX` (`tests/rfc0110.rs:373`, asserted at `:1247-1250`) with `own unique` reject fixtures across all shapes; assert `expect_err` with source-only vocabulary (no `__cap`/`__witchy` leak — the `:1317`-style check). Add an analysis `#[cfg(test)]` unit test: own-unique unproven → miss; own-unique proven (fresh value / `move`) → no miss.
- Parity: analysis-only; both `try_module_no_copy_misses` (opt errors) and the future repair path consume this ONE widened set (opt-rejects ⇔ normal-repairs, structurally).
- **Gate: run the full example/book suite under `mode opt`** before proceeding — this is the blast-radius check for newly-rejected opt programs.

**Step 4 — Add the lever-independent, source-coordinate-keyed repair accessor (analysis-only, no consumer). Risk: low.**
- Edit: `crates/witchy-lower/src/analysis.rs` near `module_no_copy_misses_with_access` (`analysis.rs:4538`). Add `pub fn module_boundary_repairs(module: &Module) -> Vec<BoundaryRepair { function: String, line: usize, callee: String, arg_index: usize }>`, computed by reusing `module_no_copy_misses_with_access` internals verbatim (a normal-mode repair *is* an opt-mode miss). Key by source coordinates — the `NoCopyMiss` already carries `function`, `line`, `callee`, `var` (`analysis.rs:4450-4508`); add `arg_index` to the miss struct so the key is unambiguous.
- Proves: analysis unit test asserting the repair set for a two-calls-on-one-line fixture is disambiguated by `callee`+`arg_index` (guards the keying hazard from the `module.clone()` at `analysis.rs:4527`).
- Parity: no consumer yet; pure derivation.

**Step 5 — Thread the repair set + fire the boundary counter at the existing zero-token arm (codegen). Risk: med.**
- Edit: `crates/witchy-lower/src/codegen/mod.rs`. Compute the repair set once in `Codegen::new` (near `mod.rs:1586`) from `self.checked_module`. At the three capacity-slot sites — the closure/indirect var arm (`mod.rs:2501-2521`), the direct owned arm `owned_argument_cap` (`mod.rs:4332-4340`), and the second owned-cap site (`mod.rs:6963-6964`) — when `(cur_function, cur_line, callee, arg_index) ∈ repair_set`: keep the **existing `ConstI32(0)` arm** (already the behavior) and emit exactly one `Self::increment_counter("__witchy_boundary_reown_copies")` (`mod.rs:8887`) before the `CallStoreMulti`. Also increment `__witchy_ownership_token_repairs` once at the same site (the logical repair event).
- **CRITICAL parity note:** the counter fires off `repair_set` membership, **NOT** off `inplace_push.contains(root)` — otherwise `WITCHY_OPT=none` (empty `inplace_push`) makes every arg take the zero arm and over-counts. This is the criterion-9 lever-invariance guarantee.
- Interpreter: **no functional change.** Add a comment anchor at the `eval_call_args` Var/Own bind path (near `interpreter.rs` `writebacks`/`try_inplace_assign`, `:490`/`:1443`) documenting that `Rc::make_mut` COW *is* the oracle the compiled zero-token copy reproduces.
- Proves: convert the Step-2 anchor fixtures into **paired** tests — (a) `mode opt` twin `expect_err`s (already from Step 3); (b) mode-less twin runs value-equal across all levers AND asserts `boundary_reown_copies() == 1`. Add: an accepted-opt no-copy program (proven-unique arg) asserts `boundary_reown_copies() == 0` (criterion 9 "zero for accepted opt"). Add a **lever-invariance** test: one repaired fixture reads the *same* `boundary_reown_copies()` on `OptSet::all`, `OptSet::none`, and every single-lever-off.
- **This flips ledger rows 2 (repair half), 8, 9 (boundary counter) toward PROVEN.**

**Step 6 — Local-unique escape gate (both-modes error, reuse existing oracle). Risk: med.**
- Edit: in the mode fork `src/lib.rs` (`enforce_performance_modes`, `:209`) and/or the repair-set producer: before treating a `local unique` miss as a normal-mode repair, if the escape oracle proves the repaired value escapes the activation, emit a **hard error in BOTH modes**. **Reuse** `crates/witchy-types/src/loans.rs:111` `authenticated_borrow_escape_boundary` + `typeck.rs:474` `is_local_unique_type` / `:1523` return-escape rule. Do **not** write a new escape oracle (RFC lines 177-178).
- Proves: a `local unique` arg whose repaired copy would escape → `expect_err` in both `mode opt` and mode-less; a non-escaping `local unique` repair → compiles, value-equal, counter `== 1`.
- Parity: the checker rejects before either backend runs, so the interpreter never executes an escaping local-unique repair (already true today via typeck; this pins it).

**Step 7 — Drop the entry-function filter for the no-copy check only. Risk: HIGH.**
- Edit: `src/lib.rs:236-237` — remove the `is_entry_function` filter (`:237`) *for the `try_module_no_copy_misses` loop only*. Leave the loop-cliff and FIP filters (`:262`, `:275`) entry-gated. Criterion 2 = "every source-facing unique parameter at **every** call shape," and `mode opt` is transitive across imports (`linker.rs:1864`); enforcement must cover non-entry helpers.
- Proves: a fixture with an unproven-unique call in a **non-entry helper** under `mode opt` now `expect_err`s; its mode-less twin repairs (counter `== 1`).
- **STOP-3 (see below): this is the largest opt-mode blast radius.** Run the full example/book/projects suite under `mode opt` and enumerate every newly-rejected site as an *intentional* paired fixture before landing. If a newly-rejected site is a false positive (a genuinely-provable arg the widened proof misses), **STOP and fix the proof, not the filter.**

---

## PHASE C — direct-storage `var` lowering (criterion 6; ledger row 6, and row-9 direct-storage counter)

**Step 8 — Add the single de-opt lever. Risk: low.**
- Edit: `crates/witchy-syntax/src/opt.rs` — add `Opt::DirectStorageVar` to the enum (`:51`), to `ALL` (`:127`, `[Opt; 13]`→`[Opt; 14]`), and `name()` (`:144`). Wire to `force_copy_mode` like existing memory-behavior levers. Update the `default_release`/`OptSet::all` assertions in `opt.rs` tests (`:357-409`) to include it.
- Proves: `opt.rs` unit tests green; the `tests/rfc0110.rs:240` single-lever sweep now includes it (must fire a real counter, not a phantom no-op — satisfied in Step 10).
- Parity: lever off ⇒ current move-in/write-back verbatim — the de-opt oracle for the whole phase.

**Step 9 — Six-proof eligibility gate (analysis + access facts, no lowering change). Risk: med.**
- Edit: a new `fn direct_storage_ok(&self, place, access, arg_index) -> Result<(), MissingProof>` in `crates/witchy-lower/src/codegen/mod.rs`, consuming ONLY facts codegen already holds:
  - **P1 evaluated-once:** `capture_codegen_place` (`mod.rs:7182`) already lowers each projection coordinate once into a scratch prelude; require no re-evaluated dynamic coordinate.
  - **P2 overlap-disjoint:** `crates/witchy-types/src/access.rs:122` `CheckedPlace::overlaps` + `checked_place_facts` (`access.rs:206`). Decline (fall to move-in) if `overlaps()` or `has_dynamic_index()` — **fail-closed, never guess.**
  - **P3 no live alias/view:** the same escape/loan substrate feeding `inplace_push` (`analyze_with_access`, `analysis.rs:1081`; consumed at `mod.rs:4363`) + `loans.active_at`. Decline on live whole-alias/active loan.
  - **P4 no callee escape:** access-signature escape summary (`OwnershipStateFlow`, `access.rs:774-803`). Decline on escape.
  - **P5 identical-repr:** the `wir_convert`-is-no-op condition already computed near `mod.rs:7452` (`param_kinds[i] == ak`). Decline on mismatch.
  - **P6 valid-writeback:** accept ONLY a **whole local** (root, no projection steps) with a scalar/reference `OwnershipStateClass` and valid final ownership state.
- Proves: analysis/codegen unit tests — disjoint whole-local → `Ok`; overlapping/dynamic-index/aliased/escaping/kind-mismatch/projected → `Err(MissingProof::{P1..P6})`.
- **No AST-shape recognizer, no new `*_cap` helper** (CLAUDE.md / RFC-0051). Every proof reads a shared fact.

**Step 10 — Slice A lowering (whole-local write-back streamlining) + direct-storage/repair counters. Risk: med.**
- Edit: `crates/witchy-lower/src/codegen/mod.rs:7388` `lower_var_call`, **before** the result-scratch reconstruction block (`mod.rs:7484-7550`). When `enabled(DirectStorageVar) && !force_copy_mode()` and `direct_storage_ok` returns `Ok` for a whole-local place: skip the reconstruct-into-`root_scratch` + `codegen_place_update_from` (`mod.rs:7278`) rebuild and write the `CallStoreMulti` var-result slot directly to the caller local via the single trailing `SetLocal`. Emit `increment_counter("__witchy_direct_storage_var_accesses")`. On the fallback (any failed proof, or lever off), keep the reconstruction verbatim and emit `increment_counter("__witchy_ownership_token_repairs")` (the deterministic complement — "reconstructed rather than forwarded").
- **Trap atomicity (parity-critical):** the accepted path still commits via a single `SetLocal` **after** `CallStoreMulti` returns (`mod.rs:7548-7550`), so a trapped VM commits nothing — identical to move-in. **This path does NOT mutate caller storage inside the callee.** That is Slice B — DO NOT implement here (STOP-4).
- Opt-mode "reports which proof is missing": in `mode opt`, when `DirectStorageVar` is requested and `direct_storage_ok` returns `Err(MissingProof::Pn)`, surface a new miss (mirror `NoCopyMiss` shape) through `enforce_performance_modes` (`src/lib.rs`) naming the proof, source-only vocabulary.
- Proves: (accept) whole-local six-proof fixture → value-equal lever-on vs lever-off vs interpreter, `direct_storage_var_accesses() >= 1` (on) / `== 0` (off, with `ownership_token_repairs()` incremented as complement). (decline) six fixtures each breaking exactly one proof → move-in fallback, value-equal, `direct_storage_var_accesses() == 0`, and under `mode opt` a proof-named rejection. Trap-mid-var-call fixture → caller local unchanged on both backends.
- **This flips ledger row 6 (via the six-proof gate + streamlined write-back) and row 9 (direct-storage counter).**
- Parity: interpreter has no direct-storage mode (always move-in/commit); Slice A is observationally identical on every non-trapping path (RFC lines 88-89).

---

## PHASE D — close-out

**Step 11 — Surface real counters in `witchy stats`. Risk: low.**
- Edit: `src/stats.rs:35-39,96-98` already plumb the three counters via `unwrap_or(0)`. Update the `stats.rs:129-133` test from a blanket `== 0` to assert the criterion-9 contract on representative fixtures (accepted-opt-no-copy → 0; repaired-normal → 1; direct-storage accept → `>= 1`). `src/dispatch.rs:157-159` print path already exists.
- Proves: `witchy stats` prints real values; test encodes the deterministic contract.

**Step 12 — Docs + ledger (LAST, only after 1-11 green). Risk: low.**
- Edit: `spec/language.md` + `spec/performance.md` + one runnable `book/` example describing the shipped normal-mode one-copy repair, the six-proof direct-storage lowering, and the four/five deterministic counters — **as what IS**, no history framing (CLAUDE.md). Flip `rfcs/0110-0112-acceptance-ledger.md` rows 2/6/8/9/10 to **PROVEN** with the exact new test names. Regenerate any generated manifest (`spec/stdlib.md` is untouched here).
- Proves: executed `book/`/spec `\`\`\`witchy\`\`\`` blocks run on both backends; `stdlib_docs_are_current`-style doc-currency tests green.

---

## STOP-if-uncertain boundaries (hard stops that risk the ownership model)

- **STOP-1 (foundational thesis):** If any Step-2 anchor fixture diverges *today* (interpreter vs. any lever) in normal mode, the whole minimal-repair thesis is invalid — it would mean an in-place mutation path exists that ignores `{name}__cap`. Do not proceed; the repair would need Design 2/3's eager boundary copy on both backends. This is the single most important gate. (The parity suite being green on master strongly implies token-gating is complete, but the fixtures must confirm it for the widened shapes.)
- **STOP-2 (coverage semantics):** Do NOT widen to `let unique` or `bare unique` without first confirming their `Convention→AccessKind` mapping (`access.rs:36`) and whether a re-own *copy* is even meaningful for a non-mutating convention. `let` is `SharedBorrow` — a copy-repair is semantically wrong there. Confirm the default convention of a prefix-less `unique T` param before touching it.
- **STOP-3 (opt blast radius, Step 7):** If dropping the entry filter rejects a site whose arg is genuinely provable, fix the *proof* (`NoCopyWalker`), never re-narrow the filter. A false-positive opt rejection is a broken contract, not an acceptable behavior change.
- **STOP-4 (Slice B / trap atomicity):** Do NOT implement the callee-mutates-caller-storage pointer ABI (the aggressive direct-storage of `mod.rs:4244-4316` epilogue + `mod.rs:3830-3847` signature split + early-return replication at `block_lower.rs:485-529`/`mod.rs:7701`). A mid-call trap leaves partial caller mutation while the interpreter shows the pre-call value — sound *only* if the VM is provably terminal on every host surface (`Runtime::run`/resume/inspect/worker). That host-surface audit is a prerequisite deliverable, not a lowering detail. Ship Slice A; leave Slice B as a separately-gated future RFC increment.
- **General:** Any step that would clone/rewrite the analyzed AST between analysis and codegen breaks the `&Stmt`-pointer keying of the existing `kills`/`dirty` facts (`analysis.rs:73`) — the repair set sidesteps this by source-coordinate keying, but do not introduce a new pointer-keyed consumer.

## Parity-preservation summary (the one rule)

The interpreter (`try_inplace_assign` / `Rc::make_mut` / `Rc::try_unwrap`, `interpreter.rs:1443-1543`) is the unchanged oracle throughout Phase B. Normal-mode repair reproduces its COW at the compiled source boundary via the pre-existing zero-token path — **value-parity is structural, not incidental**. Every repaired/direct-storage fixture is validated on interpreter + `OptSet::all` + `OptSet::none` + every single-lever toggle against an independent expected oracle (the established `tests/rfc0110.rs` pattern). The boundary counter is lever-invariant by construction (driven by the checked-access repair set, not `inplace_push`). Opt-rejects ⇔ normal-repairs because both consume the identical `module_no_copy_misses_with_access` set — no second AST-shape ownership engine (RFC line 234).

Relevant files: `crates/witchy-lower/src/analysis.rs`, `crates/witchy-lower/src/codegen/{mod.rs,assembly.rs}`, `crates/witchy-types/src/{access.rs,loans.rs,typeck.rs}`, `crates/witchy-interp/src/interpreter.rs`, `crates/witchy-syntax/src/opt.rs`, `crates/witchy-runtime/src/runtime.rs`, `src/{lib.rs,stats.rs,dispatch.rs}`, `tests/rfc0110.rs`, `rfcs/0110-opt-ownership-access-abi.md`, `rfcs/0110-0112-acceptance-ledger.md`.
