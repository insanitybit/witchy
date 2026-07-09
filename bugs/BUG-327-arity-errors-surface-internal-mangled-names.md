# BUG-327: Arity errors surface internal mangled names (Type__method, Trait__Type__method, PartialOrd__T__m) and mono-suffixed locations (fn__Int) instead of source-level names

Severity: MED
Status: FIXED
Verified: 2026-07-08 fixed on master 252af8c
Component: crates/witchy-types/src/typeck.rs, crates/witchy-types/src/traits.rs, RFC-0046, diagnostics

## Resolution

Current master routes type-error locations and arity callee names through
`diagnostic_callable_name(...)`, so user-facing diagnostics render method and
trait dispatch symbols as surface names:

- inherent method: `Point.scaled`, not `Point__scaled`;
- trait/static method: `Person.hi` / `Score.zero`, not lowered impl symbols;
- generic trait dispatch: enclosing location `smallest`, not `smallest__Int`,
  and callee `Int.less`, not `PartialOrd__Int__less` or equivalent.

RFC-0072 diagnostic goldens lock the behavior. The focused verification is:

```sh
CARGO_TARGET_DIR=target-codex cargo test arity_uses_surface_name -- --nocapture
```

Snapshots checked:

- `inherent_method_arity_uses_surface_name`
- `static_method_arity_uses_surface_name`
- `trait_method_arity_uses_surface_name`
- `generic_trait_dispatch_arity_uses_surface_name`

## Historical Problem

Errors should name what the user wrote. Method-call surface is documented as
`value.method(args)` (`book/src/tour-functions.md:25-46`), and
`Trait__Type__method` is explicitly an internal lowering name ("Mangled name for
an impl method: `Trait__Type__method`", `crates/witchy-types/src/traits.rs:32-39`).
BUG-208 already set the project standard that desugar synthetics must not surface
in diagnostics; the same standard applies to method/trait/mono mangles.

Four distinct mangled-shape leaks in arity errors, each reproduced twice:

- inherent method, wrong arity: "`t_arity_method.Point__scaled` expects 2
  argument(s) but got 3";
- trait method: "`Greet__t_arity_trait.P__hi` expects 1 argument(s) but got 2";
- static method: "`t_static_arity.Score__zero` expects 0 argument(s) but got 1";
- trait call inside a generic fn leaks BOTH the trait-dispatch symbol and the
  mono suffix on the enclosing fn: "type error: `t_arity_ord.smallest__Int`,
  line 6: `PartialOrd__Int__less` expects 2 argument(s) but got 3" — the user
  wrote `less(x, best, 1)` inside `fn smallest(...)`; neither
  `PartialOrd__Int__less` nor `smallest__Int` appears in their source.

A plain free function renders cleanly (module-qualified but not mangled), so the
leak is specific to the mangled-callee arity path. MED: diagnostics-quality on the
most common error class, plus location misattribution via the mono suffix; no
wrong behavior or parity divergence.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ D=/Users/cobrien/workspace/witchy/scratch/ultra-diag
$ $W check $D/t_arity_method.witchy
type error: `main`, line 11: `t_arity_method.Point__scaled` expects 2 argument(s) but got 3
$ $W check $D/t_arity_trait.witchy
type error: `main`, line 13: `Greet__t_arity_trait.P__hi` expects 1 argument(s) but got 2
$ $W check $D/t_static_arity.witchy
type error: `main`, line 9: `t_static_arity.Score__zero` expects 0 argument(s) but got 1
$ $W check $D/t_arity_ord.witchy
type error: `t_arity_ord.smallest__Int`, line 6: `PartialOrd__Int__less` expects 2 argument(s) but got 3

# control: t_arity_free.witchy → "`t_arity_free.add` expects 2 argument(s) but got 3" (unmangled)
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-diag/t_arity_method.witchy`,
`t_arity_trait.witchy`, `t_static_arity.witchy`, `t_arity_ord.witchy`; control
`t_arity_free.witchy`.

## Code evidence

- `crates/witchy-types/src/traits.rs:33-39` — the mangle
  (`Trait__Type__method`), documented as an internal lowering name.
- `crates/witchy-types/src/typeck.rs:2436-2443` and `:2541-2546` — the raw
  `{name}` is interpolated into the arity message with no demangling layer.
- `crates/witchy-types/src/traits.rs:2983` — `format!("{name}__{}",
  safe.join("__"))` produces the mono suffix that appears in the error prefix
  (`t_arity_ord.smallest__Int`).
- Distinct from BUG-208 (only `__kw` temps), BUG-210 (unreachable method
  defaults; quotes a `Point__scaled` error only incidentally), BUG-001 (dispatch
  correctness), BUG-247 (parser-token leak).

## Fix direction

Add a demangling layer for user-facing diagnostics: when the callee name is a
method/trait/static mangle (`Type__method`, `Trait__Type__method`), render it as
the source-level `Type.method` / `value.method(...)` form; when the enclosing
function location carries a mono suffix (`fn__Int`), strip it to the source fn
name. Route the arity messages at `typeck.rs:2436-2443` and `:2541-2546` (and the
error-prefix location) through it. Add diagnostics tests for inherent/trait/static
method arity and a trait call inside a generic fn — none should surface a `__`
mangle or a mono suffix.
