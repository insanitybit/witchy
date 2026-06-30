# Overnight autonomous work report

Running log of autonomous cleanup/improvement work on witchy + coven.
Newest entries at the top. Each entry: what changed, why, how verified.

Green gate for every commit: `cargo build` + `cargo clippy --workspace
--all-targets -D warnings` + `witchy fmt --check` + `cargo nextest run
--workspace` (1022 tests) + wasm build. Parity (interpreter == compiled WASM)
is the prime directive.

Concurrent-agent files left untouched: `README.md` (root), `book/`,
`projects/coven-web/`, `projects/glamour/`, `external-refs/`.

## Current state (TL;DR)

Commits landed this session, all on `master`, each green at commit time:
- `d2357c9` — consolidate duplicate AST-query helpers (−74 lines)
- `7b7e67e` — stdlib membership/cardinality naming (dict/json/set, Rust-aligned)
- `08da45b` — fix std/future doc (not the async substrate)
- `24da335` — enable `derive(...)` in transitively-pulled std modules (linker
  fix) + semver `Version` → `derive(Ord)`; parity verified both backends
- `6bbf4cd` — check in proptest regression seed guarding the derive-in-std fix
- `aef3e8f` — duration: add `to_minutes/to_hours/to_days/to_weeks` (the total
  conversions were asymmetric with the unit constructors)
- `92b4b21` — cover those new duration conversions in the differential test
- `6ae6735` — add a differential test for `std/convert` From/Into (it was the
  one std module with zero behavioral coverage; trait blanket-impl machinery)
- `2c898ef` — parity test for IEEE float special-value formatting (inf/-inf/
  NaN/-0.0) — high-risk divergence area, previously uncovered
- `2dda4a2` — parity test for char-indexed substring across multibyte/emoji
- `92c741d` — captured a real PARITY VIOLATION as an `#[ignore]`'d test
- `9ee1c46` — **✅ FIXED that parity violation** (compiled-dict corruption)
- `410d676` — teaching-error: module-qualified call missing its import (e.g.
  `json.stringify` with no `import json`) now says "add `import json`" instead of
  a confusing method-resolution message
- `6b04c0f` — teaching-error: ordering an unbounded generic type param now
  suggests `where T: Ord` instead of a bare "found `?`"
- `5191e4d` — teaching-error: ordering a non-`Ord` type now points at
  `derive(... Ord)` instead of leaking the `less` desugar / mis-suggesting `last`

> **✅ Found AND fixed a real parity violation (the prime-directive bug class).**
> On the COMPILED backend only, `insert("x") → remove("x") → insert("x",5)` then
> any `dict.keys`/`values`/`pairs` iteration corrupted the re-inserted entry —
> `get_or("x")` returned the default, and a plain `for e in dict.pairs(d)` loop
> silently dropped the entry (a value-sum gave 2 instead of 7). The interpreter
> oracle was always correct (`5,1,5`); the compiled backend returned `5,1,-1`.
> **Root cause:** `dict_remove` (`crates/witchy-wir/src/wir_helpers.rs`)
> allocated `count` entry slots but advanced `heap` only past the `n` surviving
> entries, leaving the `count-n` slack (which the own-ABI tracks as capacity)
> unreserved — so the next in-place insert appended into it and the following
> allocation stomped the entry. **Fix (commit `9ee1c46`):** reserve the full
> allocated capacity (`heap = new + 4 + count*16`), matching the insert/grow
> path. Validated by the now-live regression test
> `dict_remove_reinsert_then_iterate_keeps_entry` (passes both backends) + the
> full non-e2e suite (967 tests, no regressions). Found via parity probing.
> Gotcha that hid it during investigation: plain `witchy <file>` runs the
> COMPILED backend, not the interpreter — use the `interp()` test helper for the
> oracle.

### Parity-bug hunt (while cmp.* is blocked)

Actively probed the classic dual-backend divergence areas — most are solid (added
regression tests where coverage was thin), and one turned up the compiled-dict
parity violation above:
- **Integer** negative division/modulo (truncate-toward-zero; modulo sign
  follows dividend) and overflow wrapping at i64 bounds — identical both
  backends. Already tested.
- **Float** imprecision (`0.1+0.2`), formatting, and the IEEE specials
  (inf/-inf/NaN/-0.0) — identical; the specials were untested → `2c898ef`.
  (Noted: scientific-notation float literals like `1.0e308` don't parse on
  either backend — a language limitation, not a parity bug.)
- **UTF-8** byte-length vs char-count, and char-indexed substring across 2-byte
  (é) and 4-byte (emoji) boundaries — identical; multibyte substring was
  untested → `2dda4a2`. (`to_upper` is ASCII-only on both — consistent.)

Tree is otherwise clean apart from a concurrent agent's in-flight edits to
`book/`, `README.md`, `spec/`, `external-refs/`.

Full suite: all tests pass (967 non-e2e + 57 e2e, verified after the dict fix —
incl. the now-live `dict_remove_reinsert_then_iterate_keeps_entry` regression).
The e2e tests are load-flaky (401 under contention when suites/agents run
concurrently) — verified passing in isolation; not regressions. Memory
`project_flaky_publish_e2e`.

---

## Session start: 2026-06-28

Continuing the audit-driven cleanup (`/goal`: reduce cruft, consolidate
features, ensure consistency; Rust-aligned naming, remove dead/old/stupid
things).

### Done

4. **Fix std/future doc — it is not the async/await substrate** (commit `08da45b`)
   - future.witchy claimed to be "the substrate the async/await surface lowers
     onto." Async/await actually lowers onto `std/task` (async_lower.rs emits
     `task.lazy/and_then/done/run`); `task` itself points to `future` only for
     `select`. future is a standalone racing/joining toolkit. Corrected the
     header. (future has zero real-world importers — only its own tests — but it
     provides `select`/`join_all` racing that `task` lacks, so it was kept, not
     deleted; flagged below for a product decision.)

3. **Enable derive in std modules + semver derives Ord** (commit `24da335`)
   - Found a real linker limitation: `records::lower` (derive desugaring) +
     comptime expansion ran only on the *entry* module set. A std module reached
     transitively (user `import semver` → semver `import cmp`) was pulled in
     AFTER those passes, so its `derive(...)` never desugared/generated. Derive
     on a std type worked on the CLI's full-resolution path but not the
     single-module `link_run` path the differential tests use.
   - First attempt at the fix re-ran only comptime `expand` — failed, because
     `records::lower` is what *desugars* `derive(...)` into the `meta.derive_*`
     comptime call. Reading `derive.rs` revealed the clean path: `derive::expand`
     is idempotent (consumes the annotation) and comptime auto-imports `meta`, so
     running BOTH passes on just the pulled-in modules is a no-op for derive-free
     modules and needs nothing extra in the link set.
   - Validated the linker change is a no-op for existing code (965/965 non-e2e
     tests pass with semver's derive removed), THEN re-applied the semver
     refactor: `Version` → record deriving PartialEq/Eq/PartialOrd/Ord;
     `compare`/`sign` ladder + equals/lte/gt/gte + major/minor/patch accessors
     gone (kept `compare`+`lt` as operator wrappers for pm/coven_store). Parity
     verified on both backends; full suite green (e2e confirmed in isolation).
   - This was the item I'd earlier deferred as "too risky for unsupervised
     overnight" — reframed once I realized the change is provably a no-op for all
     existing code (nothing else derives in pulled std) and fully test-covered.
     Memory `project_std_derive_link_gap` updated to RESOLVED.

1. **Consolidate duplicate AST-query helpers** (commit `d2357c9`)
   - `collect_pattern_vars`, `collect_type_names`, `collect_type_vars` each had
     two near-identical copies across crates (Vec vs HashSet sinks). Unified on
     one generic `Extend<String>` definition per query in
     `crates/witchy-syntax/src/ast.rs`; deleted 5 duplicates + the now-unused
     `is_type_var_name`. Net −74 lines. Build + clippy + 1022 tests green.

2. **Stdlib naming consistency: membership + cardinality** (commit `7b7e67e`)
   - Inconsistency found: membership was spelled `contains` (list/string/set)
     but `has` (dict) / `has_key` (json); cardinality was `length`
     (list/string) but `size` (dict/set).
   - Rust-aligned fix:
     - `dict.has` → `dict.contains_key` (matches Rust `HashMap::contains_key`)
     - `json.has_key` → `json.contains_key`
     - `dict.size` → `dict.length`; `set.size` → `set.length`
   - These are native intrinsics (the public name *is* the native-dispatch
     string, self-recursive placeholder pattern), so the rename spans the std
     wrappers + 6 Rust dispatch sites (analysis/codegen/typeck/interpreter) +
     ~35 embedded test programs + LSP completions + the fmt legacy-rewrite map.
   - Empirically verified first that `cmp.member`/`list.contains` now behave
     identically on both backends for String and user `derive(Eq)` records —
     the "compiled compares pointers" caveat in cmp.witchy is stale (codegen
     gap closed), so the membership names are pure inconsistency, safe to fix.
   - Verified: build + clippy clean, fmt clean, maze/wordcount/config_merge run
     identically on interp + sandbox. Full suite running.

### Deferred / notes — roadmap for remaining audit items

Each investigated; not done, with the reason. Most need either supervised
review (TCB/linker) or are blocked by the concurrent agent's `book/` ownership.

- **cmp.* list-helpers** (`member`/`index_of`/`count`/`unique`) — DEDUP
  INVESTIGATED AND REJECTED (the agent's `book/` is now free, so I revisited it
  properly). The audit's "these duplicate `list.*`" premise is **false**:
  `list.unique` FAILS TO COMPILE on the WASM backend for record types (`list P`
  → "construct the compiled backend does not support"), while the Eq-bounded
  `cmp.unique` compiles and works. So `cmp.unique` is NOT redundant — it's the
  version that compiles for records. (`list.contains`/`index_of` DO work for
  records, so `cmp.member`/`index_of` are individually redundant, but removing
  only some `cmp.*` while keeping `cmp.unique` is incoherent.)
  - Root cause of the limitation: `list.unique`'s internal `contains(out, x)` is
    unbounded and `out` is built from `[]`, so the `==` on a record can't recover
    its type to dispatch in compiled code (the "trait dispatch through a type var
    / call result" gap). `cmp.unique` works only because it delegates `==` to the
    separately Eq-bounded `cmp.member`. An inline `==` loop or an Eq-bounded
    `unique` calling unbounded `list.contains` both still fail.
  - PROPER FIX (not done — meaty, supervised): thread `where a: Eq` through
    `list.contains`/`index_of`/`unique` (membership/dedup need equality, à la
    Rust's `PartialEq` bound) so they compile for records; THEN `cmp.*` become
    truly redundant and removable. This changes core `list.*` signatures across
    both backends + all callers — too risky for an unsupervised/wrap-up pass.
    Until then `cmp.*` stay. The cmp.witchy comment "list.unique compares
    pointers in compiled code" is stale wording but the conclusion (use the
    Eq-bounded version for content/record correctness) still holds.
- **semver `derive(Ord)`** — see entry #3. Needs the linker reorder
  (`project_std_derive_link_gap`). Supervised.
- **future vs task overlap** — `future.witchy` (Future(a), select/join_all
  racing) and `task.witchy` (Task(m,a), the async substrate) overlap on
  poll/and_then/map. future has ZERO real importers (only its own 2 tests) but
  provides `select`/`join_all` racing that task lacks. Keep-or-delete is a
  product call (deleting removes tested unique capability) — left for the user.
  Doc now corrected (commit 08da45b).
- **`json.index` naming** — `json.get(key)` (object) + `json.index(i)` (array)
  can't merge (different signatures, no overloading) and the suggested rename
  collides. Callers are in off-limits `projects/glamour/`. Leave as-is —
  `index` for array position is intuitive.
- **`testing.fail_with` → `fail`** — INVESTIGATED, NOT a clean win. `fail_with`
  wraps a global builtin `fail` (not `testing.fail`); renaming the wrapper to
  `fail` clashes with that builtin (self-recursion/shadowing). `fail_with` is a
  fine descriptive name. Dropped.
- **duration `to_minutes/hours/days/weeks`** — DONE (commit `aef3e8f`). Trivial
  symmetric completion (whole-unit division), so worth doing.
- **math `from_hex`/`from_binary`/`from_base`** (candidate, NOT done) — `math`
  renders Int→hex/binary/base-N but can't parse back. A real asymmetry, but
  adding parsers means validation + error-handling design and there's no current
  caller — speculative feature work, not a clean consistency fix. Out of scope
  for this cleanup pass; noted for a future deliberate decision.

### Follow-up checks after the derive-in-std fix

- Surveyed std for other hand-rolled equality/comparison that `derive` could now
  replace: **semver was the only one.** cmp's internal `lt` is the generic
  `where x: Ord` helper; the other std record types (http.Request/Response,
  meta.*, server.Route, etc.) don't need comparison. So derive-in-std is a
  complete fix, not the start of a sweep.
- Full deterministic green gate re-confirmed on the committed state: build,
  clippy `-D warnings`, `witchy fmt --check` (all std + examples), the
  wasm32-unknown-unknown lib build (with check.sh's toolchain-forcing), and the
  test suite (965 non-e2e + 57 e2e in isolation).

### Stdlib audit conclusion

The stdlib is in good shape after the membership/cardinality fix. Verified
consistent: conversion families (`to_`/`as_`/`from_`, Rust-aligned — json's
`as_*` extractors match `Value::as_*`), predicates (uniformly `is_*`),
`length`+`is_empty` on all four collections. `show`/`func` are legitimate
(display trait, function combinators), not dead. The "0-importers" module list
is misleading (task is injected by async-lowering, meta by derive, etc.) so it's
not a safe basis for deleting modules.
