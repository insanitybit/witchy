# BUG-304: `std/func` `on_key` docs pass trait method `less` as a value

Severity: LOW
Status: FIXED
Verified: 2026-07-08 FIXED on worktree-wt-65181-1783508134
Component: `std/func.witchy`, generated `spec/stdlib.md`, RFC-0050

## Resolution

`std/func.witchy` no longer documents `func.on_key(less, age)`. `less` is a
trait method, and RFC-0050 deliberately excludes trait methods from value
position. The example now uses a lambda comparator:

```witchy
list.sort_by(people, func.on_key(fn(a, b): a < b, person_age))
```

`spec/stdlib.md` was regenerated from the stdlib docs source.

## Validation

```console
$ CARGO_TARGET_DIR=target-codex cargo test -p witchy stdlib_docs_are_current -- --nocapture
test example_tests::stdlib_docs_are_current ... ok
```
