# BUG-336: Book operator table and spec promise `d[k] +=` compound assignment, but dict subscript read is unimplemented (RFC-0022 punt) and fails with a misleading "in call to `list.at`" error

Severity: MED
Status: FIXED
Verified: 2026-07-08 TEST on master dbebffb
Component: crates/witchy-syntax/src/parser.rs, crates/witchy-types/src/typeck.rs, book/src/appendix-operators.md, spec/language.md, RFC-0022, diagnostics

## Problem

Current source implements the promised behavior. Dict subscript reads lower to
`dict.at` once the receiver type is known, so `d[k]`, `d[k] += v`, and nested
dict/list place assignment work on both backends. The book and spec rows are now
truthful rather than over-claims.

## Historical Problem

`book/src/appendix-operators.md:19` (executed-docs surface): the row "`xs[i] =
v`, `d[k] = v`, `x.f = v` | assign to a place — … Compound `+=` etc. work"
claims compound assignment works for the whole family including the dict place.
`spec/language.md:149-154` puts `d[k] = v` in the same place-assignment family
and closes with "Compound forms (`xs[i] += v`) work too." But dict subscript
READ is unimplemented — RFC-0022 (`:178-180`) deliberately punts compound-on-dict
until dict subscript-read lands.

`var d = dict.new().insert("a", 1); d["a"] += 2` → "type error: `main`, line 3:
in call to `list.at`: expected `List(?)`, found `Dict(String, Int)`". Identical
error for an Int-keyed dict, for a bare subscript read `let v = d["a"]`, and for
the manual read-modify-write `d["a"] = d["a"] + 2`. The desugar of the compound's
subscript-READ blindly emits `list.at` regardless of receiver type, so the
diagnostic leaks the internal desugar and misattributes a call the user never
wrote. Compound works on the other two family members (list, record) and plain
`d[k] = v` works on dict — only the dict compound/read leg is missing.

The RFC punt is fine, but the book table and spec contradict it, and the error
message misleads. MED: loud (not HIGH), but a two-doc-surface contract violation
plus an actively misleading compiler diagnostic.

## Repro

```sh
$ W=/Users/cobrien/workspace/witchy/target-claude/release/witchy
$ $W check scratch/ultra-spec/t_dict_compound_min.witchy
type error: `main`, line 3: in call to `list.at`: expected `List(?)`, found `Dict(String, Int)`
# same for t_dict_read_subscript.witchy (bare d["a"]) and t_dict_read_expr.witchy (d["a"] = d["a"] + 2)

# controls (parity agree): t_dict_plain_assign.witchy (d["b"] = 7 on dict works),
#   t_list_compound_ctl.witchy (xs[1] += 5), t_field_compound.witchy (acct.balance += 50)
```

Probes: `/Users/cobrien/workspace/witchy/scratch/ultra-spec/t_dict_compound_min.witchy`,
`t_dict_read_subscript.witchy`, `t_dict_read_expr.witchy`,
`t_dict_compound_int_key.witchy`; controls `t_dict_plain_assign.witchy`,
`t_list_compound_ctl.witchy`, `t_field_compound.witchy`.

## Code evidence

- Filing-time `crates/witchy-syntax/src/parser.rs:2490` — lowered every
  `base[index]` to `list.at(base, index)` unconditionally.
- Filing-time `crates/witchy-types/src/typeck.rs:2310` — typed it as that call,
  so the diagnostic leaked the internal desugar and misattributed a call the user
  never wrote.
- `rfcs/0022-index-assignment.md:107-110` — "today only `list` has subscript
  read"; `:177-180` deliberately punts compound-on-dict.
- Docs promising the behavior: `book/src/appendix-operators.md:19`,
  `spec/language.md:149-154`.
- Related to BUG-317 (nested place-assign through a dict in the middle — same
  no-dict-subscript-read root, different symptom).

## Fix direction

Two threads. (1) Docs: correct `book/src/appendix-operators.md:19` and
`spec/language.md:149-154` to state that dict compound assignment (`d[k] += v`)
and dict subscript READ are not yet supported (use `dict.get`), matching the
RFC-0022 punt — until subscript read lands. (2) Diagnostic: make the subscript-read
desugar (`parser.rs:2490`) receiver-aware or emit "subscript read is not defined
on Dict — use `dict.get`" instead of leaking `list.at(Dict…)`. The real fix
(implementing dict subscript-read) would satisfy the docs and also unblock BUG-317
and the compound form. Add a test for the diagnostic and, if implemented, a
`d[k] += v` differential test.

## Fixed Evidence

- `crates/witchy-types/src/typeck.rs` now lowers `Expr::Index` by receiver type:
  `dict.at(d, k)` for `Dict`, otherwise `list.at(xs, i)`.
- `src/example_tests.rs::dict_subscript_read_and_nested_place_assignment_work_on_both_backends`
  passes on both interpreter and compiled WASM.
- A fresh parity repro for `d["a"] += 2` and
  `nested["outer"]["inner"] = nested["outer"]["inner"] + 3` agrees across both
  backends.
