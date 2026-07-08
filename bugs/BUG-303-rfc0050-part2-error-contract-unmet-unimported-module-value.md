# BUG-303: RFC-0050 value-position diagnostics leak `unbound variable`

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on worktree-wt-65181-1783508134
Component: `crates/witchy-types`, RFC-0050 method-call generalization diagnostics

## Resolution

The report had two parts:

- `let f = iter.count` without `import iter` now uses the same missing-import
  teaching diagnostic as call position.
- `let f = show`, where `show` is a trait method, now reports that trait methods
  have no single first-class function value and names the lambda spelling:
  `fn(x): x.show()`.

This closes the RFC-0050 diagnostic contract without changing the language rule:
trait methods are still excluded from value position.

## Evidence

- `crates/witchy-types/src/typeck.rs` carries pre-lowering trait method names
  through trait lowering so the value-position checker can replace the generic
  unbound-variable fallback.
- `crates/witchy-types/src/typeck_tests.rs` has
  `trait_method_value_position_names_the_lambda_fix`.

## Validation

```console
$ CARGO_TARGET_DIR=target-codex cargo test -p witchy-types trait_method_value_position_names_the_lambda_fix -- --nocapture
test typeck::tests::trait_method_value_position_names_the_lambda_fix ... ok

$ CARGO_TARGET_DIR=target-codex cargo run --quiet -- check =(printf 'import show\n\nfn main(console: Console):\n    let f = show\n    print(console, "x")\n')
type error: `main`, line 4: trait method `show` has no single function value to reference — wrap the receiver dispatch in a lambda, e.g. `fn(x): x.show()`
```
