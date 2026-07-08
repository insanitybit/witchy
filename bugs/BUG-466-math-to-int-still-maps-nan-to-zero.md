# BUG-466: `math.to_int(NaN)` aborts instead of returning zero

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-bug-466-close`
Component: `std/math`, numeric builtins, RFC-0044 error policy, interpreter/compiled parity

## Summary

`math.to_int` no longer silently maps `NaN` to `0`.

The conversion contract is now:

- finite floats truncate toward zero;
- out-of-range finite values and infinities keep the documented saturating
  conversion behavior; and
- `NaN` is a loud runtime contract error:
  `math.to_int: NaN cannot be converted to Int`.

This removes the silent-default path where a failed floating calculation could
become a plausible integer index, count, timestamp component, or money amount.

## Evidence

Fixed by `80fd2cb3` (`math: reject NaN in to_int`).

Implementation points:

- `crates/witchy-interp/src/interpreter.rs` rejects `Value::Float(x)` when
  `x.is_nan()` before applying Rust's saturating cast.
- `crates/witchy-wir/src/wir_helpers/mod.rs::float_to_int_helper` emits the
  same NaN guard and routes the compiled backend through the shared
  `DiagTemplate::NanToInt` abort path.
- `std/math.witchy` and `spec/stdlib.md` document NaN as a runtime error while
  preserving finite/infinite saturation.

Regression coverage:

- `example_tests::math_to_int_nan_aborts_on_both_backends` checks the
  interpreter and compiled backend abort on `math.to_int(0.0 / 0.0)` with the
  stable diagnostic, while `3.9`, `-3.9`, `inf`, and `-inf` still produce the
  expected truncating/saturating outputs.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy math_to_int_nan_aborts_on_both_backends -- --nocapture
```

Result: passed, 1 test.
