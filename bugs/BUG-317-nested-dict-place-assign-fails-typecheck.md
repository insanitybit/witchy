# BUG-317: Place-assign through a Dict in the middle of a nested path (d[k1][k2] = v, d[k][i] = v) fails to type-check — parser desugars the intermediate place READ to list.at

Severity: MED
Status: FIXED
Verified: 2026-07-08 TEST on master dbebffb
Component: crates/witchy-syntax/src/parser.rs, RFC-0028 mutable value semantics, RFC-0022 index assignment, place-assignment desugar, diagnostics

## Problem

Current source implements type-directed subscript reads in typeck: list
subscripts lower to `list.at`, Dict subscripts lower to `dict.at`. The regression
`dict_subscript_read_and_nested_place_assignment_work_on_both_backends` covers
direct dict reads, `d[k] += v`, nested `Dict(String, Dict(...))` writes, and
Dict-of-List writes on both backends.

## Historical Problem

RFC-0028 (status: implemented, `rfcs/0028-ergonomic-mutable-value-semantics.md:84-86`):
"Assignable place is exactly RFC-0022's set: a `var` binding, a subscript
(`xs[i]`, `d[k]`), or a field (`a.b`), nested freely." RFC-0022
(`rfcs/0022-index-assignment.md:141-147`) specifies the nested desugar `g =
g.set_at(i, g.at(i).set_at(j, v))`. `spec/language.md:150` lists `d[k] = v` among
the places. Nothing scopes nesting to lists only.

Any nested place that requires reading THROUGH a Dict subscript fails:
`d["outer"]["inner"] = 7` → "type error: cannot resolve the method call
`.set_at(…)` — the receiver's type is not known here; call the function directly:
`set_at(value, …)`" (exit 2). A full type annotation `var d: Dict(String,
Dict(String, Int))` does not help. Same for a list inside a dict (`d["row"][0] =
9`). The suggested fix is a dead end: no free `set_at` exists for Dict (RFC-0022
retargets to `insert`), and the user never wrote `.set_at`.

The root cause: the place-assign desugar wraps the inner write in a `.set_at`
MethodCall on the outer READ of the place, and the subscript READ desugar is
hardcoded to `list.at` — so `list.at(Dict…)` reaches typeck and dispatch dies.
Only Dict-in-the-middle paths fail; nested lists, dict-as-final-subscript, and
deep record paths all work. MED: loud compile error, no parity divergence, but a
promised common pattern fails even with full annotations and a misleading
diagnostic.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W parity scratch/ultra-sugar/t_nested_dict.witchy
... type error: cannot resolve the method call `.set_at(…)` — the receiver's type is not known here; call the function directly: `set_at(value, …)`
# also fails with full annotation (t_nested_dict2.witchy) and list-in-dict (t_dict_of_lists.witchy)

# controls (parity agree): t_nested_list.witchy (matrix[0][1]=99), t_mixed_nest.witchy (box.d["k"]=5),
#   t_list_of_dicts.witchy (xs[0]["k"]=5), t_nested_field.witchy (o.inner.x=42), t_mixed_path.witchy (b.items[1]=99)
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-sugar/t_nested_dict.witchy`,
`t_nested_dict2.witchy`, `t_dict_of_lists.witchy`; controls `t_nested_list.witchy`,
`t_mixed_nest.witchy`, `t_list_of_dicts.witchy`, `t_nested_field.witchy`,
`t_mixed_path.witchy`.

## Code evidence

- `crates/witchy-syntax/src/parser.rs:2245-2268` — `desugar_place_assign`
  recursively wraps the inner write in a `.set_at` MethodCall on the outer READ
  of the place.
- Filing-time `crates/witchy-syntax/src/parser.rs:2490-2498` — `desugar_index`
  lowered `base[index]` to `list.at(base, index)` unconditionally, so a
  Dict-in-the-middle read fed `list.at(Dict…)` into typeck and dispatch died.
- `spec/stdlib.md:363` — the retarget-to-insert only fires "once the receiver is
  known to be a Dict".
- Compound `d[k] += v` is the documented RFC-0022 dict-subscript-read punt (not
  counted here), though its diagnostic also leaks the internal `list.at` desugar.

## Fix direction

Make the nested-place read desugar receiver-aware rather than hardcoding
`list.at`: the intermediate READ in `desugar_place_assign` should lower to a
subscript-read that dispatches on the receiver type (retargeting to `dict.get`
for a Dict, `list.at` for a List), matching how the final subscript write already
retargets `set_at`→`insert` for a Dict. This requires the dict subscript-read to
exist (currently only lists have it — `rfcs/0022-index-assignment.md:107-110`), so
implementing dict subscript-read is the enabling step and would also close the
`d[k] += v` compound punt. Add tests: `d["a"]["b"] = 7` and `d["row"][0] = 9`
must compile and agree on both backends. (Overlaps the misleading `list.at`
diagnostic with the spec-contract finding on `d[k] +=`.)

## Fixed Evidence

- `crates/witchy-types/src/typeck.rs` now lowers `Expr::Index` by receiver type:
  `dict.at(d, k)` for `Dict`, otherwise `list.at(xs, i)`.
- `src/example_tests.rs::dict_subscript_read_and_nested_place_assignment_work_on_both_backends`
  passes on both interpreter and compiled WASM.
