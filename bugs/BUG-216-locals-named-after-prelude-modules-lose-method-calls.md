# BUG-216: locals named after prelude modules lose method calls

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Component: `crates/witchy-syntax/src/linker.rs`, method-call name resolution

## Problem

`x.f(args)` resolved inconsistently when `x` was a local variable with the same
name as a prelude or imported module. Value position let the local win
(`list.length` was a field read), but call position could still resolve to the
module (`list.length()` as `list.length(...)`). The worst case was silent:

```witchy
let string = S("s")
string.to_upper("x")
```

When `string.to_upper` had the same arity as the std module function, the local
receiver was ignored and the call became `std/string.to_upper("x")`.

## Resolution

Dotted calls now follow the same shadowing rule as dotted values. If the base
identifier is bound locally, `base.method(args)` is rewritten to a method call on
that local and left for trait/UFCS lowering. It no longer keeps an arity-based
module-call exception.

The old exception existed mostly to preserve the ambiguous double-receiver Rand
idiom (`rand.hex(rand, n)`). `Rand` now participates in ordinary receiver-method
lowering through `std/rand`, so the coherent spelling is `rand.hex(n)`. The
in-tree `coven-web` call sites were updated.

Regressions:

- `shadowing_module_name_keeps_dotted_calls_on_local`
- `rand_capability_supports_std_method_syntax`
