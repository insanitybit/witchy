# BUG-214: bare `Nil` expressions compile on both backends

Severity: HIGH
Status: FIXED
Fixed: 2026-07-06
Component: `Nil`, type checker, compiled backend

## Problem

`Nil` is the language's unit value, but the parser represents uppercase bare
names as constructor expressions. The type checker let `Nil` fall through the
unknown-constructor path as an unconstrained fresh type, while the compiled
backend only knew how to lower declared data constructors.

That meant ordinary unit expressions could pass earlier phases but fail on the
compiled backend when used as a tail expression, a statement, a match-arm value,
or through a `Nil`-returning helper.

## Fix

The type checker now recognizes `Nil` as the built-in unit value and rejects
field arguments such as `Nil(1)`. WIR lowering maps zero-argument `Nil` to the
same `i32.const 0` unit representation already used for implicit unit values.

Regression:

- `bare_nil_expression_compiles_on_both_backends`
