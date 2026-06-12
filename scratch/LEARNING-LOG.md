# LEARNING-LOG — round 5, ambitious mode

Each entry: what I tried, what I expected, error verbatim (or behavior), what fixed it, severity.
Severities: Blocker / Friction / Papercut / Worked-well.

## Setup

- witchy binary already built at `target/debug/witchy`.
- Read all book chapters + appendices + the example projects (todo, ledger, report, dashboard, config) before writing code.
- Key surfaces I will exercise: multi-module path-deps, multi-actor topologies, traits w/ bounds + derive, JSON round-trip, time/duration, comptime emit, capability narrowing through several layers, sandbox + args, big-data parse/transform.

## Summary (filled in at the end)

- **21 programs total** (20 single-file scripts: `01_`–`21_` minus `04_` which is the megaproj, plus the megaproj's 5 runes wired together).
- **First-try successes**: 12 of 21 ran clean on the first attempt (01, 05, 06, 07, 08, 11, 12, 14, 15, 17, 18, 21). The other 9 hit at least one friction or blocker.
- **All 20 single-file programs are parity-clean after fixes**, and the megaproj's output is byte-identical pre- and post-fmt.
- **Blockers found**: 2 — (a) **interpreter step-budget vs WASM** divergence on a 5000-element iterator workload (program 09); (b) **actor `Int` field without default** rejected by the WASM codegen but accepted by the interpreter (program 13). Both are loud (parity reports DIVERGE and the codegen error names the field), not silent-wrong.
- **fmt behavior change**: zero — every formatted program produced identical output to the pre-fmt run (verified by diff in the megaproj case).

## Top frictions (one line each)

1. Interpreter step-budget (~1M) hard-limits programs that the WASM backend runs to completion → parity reports DIVERGE for programs that are merely "too big" — no flag to raise it.
2. Actor `Int` field without `= default` parses+runs on interpreter but errors in WASM codegen — feels like an undocumented constraint.
3. `where a: Trait` resolves trait calls on direct params + for-loop vars, NOT on `match`-pattern bindings, NOT on call results — the error is "unknown function" with no signpost.
4. Reserved keywords (`sink`, `region`, etc.) collide with intuitive field/parameter names — error is generic ("expected identifier"), with no hint that the name is reserved.
5. `match` no-op arm: there's no `pass`/`()` idiom shown anywhere; `None -> {}` errors helpfully, but the "right" thing to write is unclear; I used `None -> out = out`.

## Entries

### 13_worker_pool — Blocker (real backend divergence)
- Wrote `actor Worker:\n    id: Int\n    collector: Subject` (positional-no-default Int field). Interpreter accepted it. WASM codegen errored: `codegen error: field 'id': Int state needs an initializer in codegen`. **Blocker**: this is a language-level backend divergence — an actor declaration that the interpreter compiles & runs is rejected by the WASM compiler. The error names the right field but no signpost in the book mentions this constraint.
- Workaround: convert `id` from constructor positional → message parameter (`on Work(my_id: Int, x: Int)`); spawn no longer needs an `id`. Lost a bit of expressivity (each worker no longer "knows its own id" privately) but functionally equivalent. Adding `var id: Int = 0` would have made it a default field — removed from the constructor entirely; can't set per-instance.
- Output bug (mine): drain ordering across actors — same cross-actor FIFO gotcha as program 03. Fixed by routing Drain through each Worker and counting 3 drains in Collector before reporting.

### 19_csv_pipeline — Friction (reserved word)
- Named a record field `region: String`. Error: `parse error at 22:5: expected an identifier, found 'region'`. `region` is a reserved word (Performance appendix: "Regions: scoping your allocations"). Renamed all to `area`; works. **Papercut**: error message doesn't say "reserved keyword" — same shape as the `sink` collision in program 3. Listing reserved words near the keywords table would help (the table on the appendix-operators page lists `actor`, `on`, `spawn` and many others, but not `region`, `sink`, `move`, `own` — well `sink`/`own`/`move` ARE there. `region` may be reserved only in some contexts).

### 16_generic_linked — Friction (trait dispatch on match-pattern vars)
- Wrote `match l: LCons(x, rest) -> weight(x) + weights(rest)` in a `where a: Counter` fn. Error: `call to unknown function weight`. **Friction**: trait dispatch resolves on direct params and for-loop vars, NOT on match-pattern bindings (consistent with the documented constraint). The error message is the same as for any unbound function — it doesn't mention the dispatch limitation, so I spent a few minutes guessing whether the trait was somehow not in scope.
- Also tried calling another bounded-generic helper from within a bounded-generic function — also fails (`call to unknown function describe_one`). So the workaround must be: write the bounded function so all trait calls happen on the direct param (or for-loop var), AND the entry point is called from a concrete-type site.
- Once I converted everything to operate over `List(a)` + `for x in xs`, all calls dispatched. **Worked-well** in the end. The book's generics chapter doesn't mention this constraint — adding a sentence on it would save real time. **Papercut documentation gap.**

### 10_kv_with_tests — Papercut
- `testing.assert_eq` is `String`-only. Calling it with `Int`s gave `type error: ... expected 'String', found 'Int'`. The right tool is `testing.assert_int_eq`. **Papercut**: separate-function-per-type is a small papercut when most assertion libraries would use a polymorphic generic Eq-bounded `assert_eq`.

### 07 — duration display Papercut
- `duration.human(milliseconds(4250))` rendered `4s`, not `4s250ms`. Looking at duration.human, sub-second remainder appears dropped for non-zero higher units. Not a bug per spec (book says human form is "30s"/"1m30s"), just less precise than `clock` or raw ms. **Papercut** if you wanted sub-second precision in human form.

### 09_big_gen — Blocker for the parity tool (with workaround), then Worked-well
- Wrote a generator (`gen fn lcg`) feeding 5000-element iterator pipelines. Interpreter aborted: `runtime error: '09_big_gen.__gen_lcg', line 12: evaluation step budget exceeded (possible infinite loop)`. WASM ran to completion and produced clean output. `parity` flagged DIVERGE.
- **Blocker (parity)**: the interpreter has a hard step budget (~1M steps, found in src/interpreter.rs). For programs that legitimately do real work — 5000 iterations through 3 nested iter pipelines easily exceeds it — interpreter + WASM diverge in OUTCOME (one errors, the other succeeds). For users this means: a programmatic workload that compiles & runs on WASM may not be runnable via the interpreter, which the book frames as semantically identical. There's no CLI flag to raise the limit. The error itself is good ("possible infinite loop") but misleading when the program is finite.
- **Severity reasoning**: it's a blocker for parity because the harness reports divergence even though the WASM result is correct. It's not a silent-wrong: parity does correctly detect the mismatch and the user is told. So a "loud divergence" rather than "silent wrong", but I'd still call it a Blocker for round-5's stated criterion that fmt/parity behavior changes are blockers — because the interpreter+compiler are supposed to agree.
- **Mitigation**: dialed inputs down to 400/200 — parity-clean. Documented limit in code comment.

### 04_megaproj — multi-rune diamond+, 4 lib + 1 app — Worked-well
- 5 runes: `core` (types) -> `parsing`/`analytics`/`formatting` -> `app`.  All path deps; `witchy tree` shows the diamond on `core` cleanly.
- `witchy new --lib NAME` and `witchy new NAME` scaffold cleanly; just needed to add `[dependencies]` entries by hand.
- **Friction**: used `None -> {}` for the no-op match arm. Error: `parse error at 13:22: braces are not part of witchy syntax — use indentation`. Great error message — tells you what's wrong AND what to use. Workaround was `None -> out = out` (clumsy). **Papercut**: there is no explicit Unit literal / pass syntax I could find. Documenting "what is the no-op arm idiom?" would help — I don't think it's in the book.
- **Friction**: I wrote `pub type Reading derive(Show, Eq):`.  fmt silently STRIPPED the `pub`.  Cross-module use of the type kept working — so `type` defaults to public, the `pub` was a syntax error that fmt salvaged. **Papercut**: book/appendix could say "types are exported by default; `pub` only applies to `fn`". Also: the compiler did not flag `pub type` BEFORE fmt — it parsed and worked. So this looks tolerant of `pub type` at parse time but fmt-normalizes it out. Not a Blocker because the program ran with identical output before and after fmt (verified by diff). 
- **Friction**: after `fmt` rewrote sources, `witchy run` blocked on a hash mismatch on the path dep: `path dependency analytics changed: hash sha256:… != locked …`. The message tells you to run `witchy update`; one command fixed it. **Worked-well** as a guard, **Papercut** as a flow when you've just `fmt`ed your own path deps. (Could plausibly be auto-relaxed when caps don't widen.)
- After update + rerun, output identical to pre-fmt — no behavior change.
- `witchy caps` reports `Console, Dir` on main as expected; `audit` shows the deps demand no caps.

### 03_actor_pipeline — Friction (mental model), then Worked-well
- Named a Subject field `sink`. Error: `parse error at 41:5: expected an identifier, found 'sink'`. `sink` is a keyword (ownership transfer). Renamed to `downstream`. **Papercut**: the error pinpoints the line but doesn't explicitly say "reserved keyword". Easy fix once known.
- Logic mistake (mine): sending `Flush` directly to `agg` from `main` while Sample messages were still queued at upstream actors produced `sum => 0`. Mailboxes are per-actor FIFO, not global. Fixed by routing the Flush through the Producer→Filter→Aggregator pipeline so cross-actor ordering is preserved. **Worked-well**: the mailbox semantics are exactly what the book describes; my bug.
- After fix: parity-clean, both backends agreed.

### 02_traits_bounds — Friction (then Worked-well)
- Tried trait method with a `where` bound: `fn plus(self, other: a) -> a where a: Summable`. Error: `parse error at 11:20: expected 'fn', found 'where'`. **Friction**: traits can't carry `where`-bound methods in their decl (or at least not with this syntax). Removed the unused `Summable` trait. Book examples never bound a trait method's own type variable; my mistake.
- After fix: parity-clean, both backends agreed. derive(Show, Eq) on records works; `${...}` interpolation renders the derived `Show`. fmt rewrote `<> to_string(x) <>` patterns into `${x}` — semantics preserved; matches the "interpolation as idiom" recent commit. **Worked-well**.

### 01_json_round_trip — Worked-well
- First try: encode/decode + nested `match` for unwrapping a `Some(JsonString(...))` worked correctly on both backends; `parity` agrees.
- fmt observation: fmt collapsed a multi-line `[..., ..., ...]` list literal onto a single long line. Not a behavior change, just a style choice. **Papercut**: very long lines are harder to read but no semantic impact.

