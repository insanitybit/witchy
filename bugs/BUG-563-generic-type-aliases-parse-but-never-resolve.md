# BUG-563: Generic type aliases parse but can never be used

**Severity:** LOW
**Status:** FIXED
**Verified:** 2026-07-09 on `fix/bug563-generic-type-aliases`
**Component:** crates/witchy-syntax (alias declaration/resolution), linker type resolution
**Found:** 2026-07-08, while probing alias semantics for RFC-0078

## Symptom

A parameterized alias declaration was accepted, but every use failed to
resolve:

```witchy
type Pair(a) = (a, a)

fn f(p: Pair(Int)) -> Int:
    p.0
```

`witchy check` reported `unknown type Pair` because the parser accepted the
head parameters but `Item::TypeAlias` dropped them, leaving alias resolution
with no substitution data.

## Fix

Generic aliases now preserve their parameter list in the AST and alias
resolution substitutes arguments structurally before erasing alias items:

```witchy
type Pair(a) = (a, a)
type Rows(a) = List(Pair(a))
```

`Pair(Int)` expands to `(Int, Int)`, and `Rows(String)` expands to
`List((String, String))`.

Alias parameters are validated like other generic declarations: duplicate
parameters are rejected, and an alias target cannot use an undeclared type
parameter.

## Verification

```sh
CARGO_TARGET_DIR=target-codex-d8 cargo test -q -p witchy-syntax -- --nocapture
CARGO_TARGET_DIR=target-codex-d8 cargo test -q -p witchy-types duplicate_declarations_are_rejected -- --nocapture
CARGO_TARGET_DIR=target-codex-d8 cargo test -q generic_type_aliases_resolve_on_linked_path -- --nocapture
CARGO_TARGET_DIR=target-codex-d8 cargo run -q -- check /tmp/bug563.witchy
```
