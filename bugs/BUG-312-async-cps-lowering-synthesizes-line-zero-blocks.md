# BUG-312: Async CPS lowering synthesizes line-0 blocks, so type errors in any `async fn` body lose their `fn`, line N: location prefix (gen fn shares the root cause)

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 64bf3332
Fixed: 2026-07-08
Component: crates/witchy-syntax/src/async_lower.rs, crates/witchy-types/src/typeck.rs, async lowering, diagnostics

## Problem

This row is stale. The bug was real when recorded: diagnostics should carry the
function and line, as every non-async path does, but a type error in an `async
fn` body used to lose its location entirely.

Current master preserves source locations through async CPS lowering. Regression
coverage checks both sides of an `await`:

- before an await: the diagnostic includes `` `main.work`, line 6: ``;
- after an await: the continuation diagnostic includes the lowered segment name
  and the original source line 7.

Focused verification on 2026-07-08:

```text
$ CARGO_TARGET_DIR=target-codex-docs cargo test async_lowered_type_errors_keep_source_locations -- --nocapture
test example_tests::async_lowered_type_errors_keep_source_locations ... ok
```

MED: loud, correct-content error; only the locator is lost — no silent wrong
behavior or parity divergence.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-chan/t_diag_async.witchy
type error: in call to `list.push`: expected `Int`, found `String`       # NO fn/line prefix
$ $W check scratch/ultra-chan/t_diag_sync_helper.witchy
type error: `t_diag_sync_helper.work`, line 3: in call to `list.push`: expected `Int`, found `String`

# also affected: t_diag_async_noawait.witchy, t_diag_async_helper.witchy (identical loss)
# control: t_diag_closure_ctl.witchy (explicit fn(k) closure in a plain fn) keeps `main`, line 4:
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-chan/t_diag_async.witchy`,
`t_diag_async_noawait.witchy`, `t_diag_async_helper.witchy`; controls
`t_diag_sync_helper.witchy`, `t_diag_ctl.witchy`, `t_diag_closure_ctl.witchy`.

## Code evidence

- `crates/witchy-syntax/src/async_lower.rs` — runs before typeck and synthesizes
  rewritten blocks with `lines: vec![0]`/`vec![0, 0]` (tail_block, prefix_stmt);
  the synthesized nodes carry no original span.
- `crates/witchy-types/src/typeck.rs:352` — `at_loc` omits the location prefix
  when `line == 0`.
- Control proves it is async-specific, not a closure-in-general issue:
  `t_diag_closure_ctl.witchy` (explicit `fn(k)` closure inside a plain fn) still
  reports `` `main`, line 4: ``.
- Distinct from BUG-162 (LSP linker diagnostics at line zero) and BUG-107
  (compiled-backend runtime abort messages). Shares the synthesized-block line-0
  pattern with BUG-295 (while-let location loss).

## Fix direction

Closed by the current async lowering behavior and
`example_tests::async_lowered_type_errors_keep_source_locations`.
