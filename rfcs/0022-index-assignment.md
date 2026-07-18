---
rfc: 0022
title: "Place-assignment sugar: d[k] = v, xs[i] = v, u.field = v"
status: implemented
created: 2026-06-28
superseded-by:
tracking:
---

# RFC-0022: Place-assignment sugar (`d[k] = v`, `xs[i] = v`, `u.field = v`)

The shipped place-assignment parser/desugar and formatter support are implemented
in [`crates/witchy-syntax/src/parser.rs`](../crates/witchy-syntax/src/parser.rs) and
[`crates/witchy-syntax/src/format.rs`](../crates/witchy-syntax/src/format.rs), with
cross-backend coverage in [`src/example_tests.rs`](../src/example_tests.rs).

> Code blocks here are intentionally **not** tagged `witchy` (per RFC-0002's
> convention): illustrative sketches, not executed by the doc-test harness. The
> proposed `d[k] = v` form does **not** parse today.

## Summary

Add assignment to a subscript **or a record field** as **pure syntactic sugar**
over the existing value-semantic update:

```
d["ada"] = 36      // desugars to:  d = d.set_at("ada", 36)
xs[0] = 9          // desugars to:  xs = xs.set_at(0, 9)
acct.balance = 5   // desugars to:  acct = Account(balance: 5, ..acct)
```

The left side must be an assignable `var`. There is **no new runtime behavior**:
the desugar reassigns the binding, which the uniqueness analysis already lowers to
an **in-place** update (O(1), no copy) when the collection is uniquely owned. This
gives the Python/Rust `d[k] = v` ergonomics that today require the longer
`d = d.set_at(k, v)` / `d = d.insert(k, v)` reassignment spelling.

## Motivation

witchy collections are immutable *values*, and an update is a reassignment:

```
var d = dict.new()
d = d.insert("ada", 36)
var xs = [1, 2, 3]
xs = xs.set_at(0, 9)
```

This is already in-place under the hood (see RFC-0016 / the uniqueness pass), so
it is cheap — but the `d = d.…(k, …)` shape reads as ceremony next to Python's
`d[k] = v` or Rust's `xs[i] = v`. `var` *does* provide mutability; what is missing
is only the spelling. Subscript-read (`xs[i]`, `d[k]` once added) already exists as
sugar for `.at` / `.get`; subscript-**write** is the symmetric gap.

## Current behavior

- `xs[i]` parses as a read: sugar for `list.at(xs, i)` ([`crates/witchy-syntax/src/parser.rs`](../crates/witchy-syntax/src/parser.rs)).
- `d["k"] = v` and `xs[0] = v` are **parse errors** ("expected an expression,
  found `=`").
- The update primitives exist: `list.set_at(xs, i, v) -> List(a)`,
  `list.update_at(xs, i, f)`, `dict.insert(d, k, v) -> Dict(k, v)`.

## Proposed semantics

A statement of the form `LVALUE[INDEX] = RHS`, where `LVALUE` is an assignable
place (a `var`, or a nested subscript thereof), desugars to:

```
LVALUE = LVALUE.set_at(INDEX, RHS)
```

resolved through the usual method (UFCS) dispatch on `LVALUE`'s type:

- **List**: `xs[i] = v` -> `xs = list.set_at(xs, i, v)`. Out-of-bounds `i` is a
  runtime error on both backends, exactly like `xs[i]` read and `list.set_at`.
- **Dict**: `d[k] = v` -> `d = dict.set_at(d, k, v)`, where `dict.set_at` is the
  upsert (the existing `insert` semantics: set `key` to `val`, preserving
  first-appearance order). See "Stdlib" below.

Assigning to a non-`var` (a `let`, or a value with no binding to write back to) is
a check-time error, identical to today's rule for plain `=`. Because the desugar
is a reassignment, it inherits in-place optimization for free and changes no
observable semantics — so **parity is automatic** (both backends compile the same
desugared AST).

### Examples (illustrative)

```
var counts = dict.new()
for w in words:
    counts[w] = counts.get_or(w, 0) + 1     // word-count, in place

var row = [0, 0, 0]
row[1] = 9
```

### Record fields

The same place-assignment idea covers a record field. `u.field = RHS`, where `u` is
an assignable `var` of a record type, desugars to the existing spread update:

```
acct.balance = acct.balance + 1
// desugars to:
acct = Account(balance: acct.balance + 1, ..acct)
```

Same rules as subscripts: the place must be a `var`; it is in-place when uniquely
owned; it is a pure desugar so parity is automatic. This removes the last
reassignment-ceremony spot — a `var` record reads like a mutable struct while
staying a value. Field *read* already exists (`u.field`); this adds field *write*,
exactly as subscript-write mirrors subscript-read. A future compound form
(`acct.balance += 1`, `d[k] += v`) would build on this once subscript-read on
`Dict` lands (today only `list` has subscript read), reusing the existing
`+=` / `-=` / `*=` tokens.

## Stdlib

To make the desugar uniform via a single `.set_at`, add `dict.set_at` as the
canonical "set key to value" name:

```
pub fn set_at(d: Dict(k, v), key: k, val: v) -> Dict(k, v)   // == insert
```

Options for `insert`/`set_at` coexistence (per the break-don't-deprecate rule, we
do not keep silent aliases):

1. **Desugar by type** — parser/typeck emits `dict.insert` for a `Dict` lvalue and
   `list.set_at` for a `List`. No new stdlib name; the desugar is type-directed.
   Slightly more compiler logic, no surface change.
2. **Unify on `set_at`** — add `dict.set_at` and desugar everything to
   `lvalue.set_at(idx, v)` uniformly. Cleaner desugar; adds one name to `dict`.
   (Whether `dict.insert` then becomes `dict.set_at` repo-wide is a separate
   call.)

This RFC leans toward (1): keep `dict.insert` as the named operation, make the
desugar type-directed. It needs no stdlib change and keeps `insert` as the word
people already use.

## Implementation

1. **Parser**: in statement position, after parsing a place expression that ends
   in one or more `[...]` subscripts, accept `= RHS` and build an
   `IndexAssign { place, index, value }` (or desugar immediately to the
   reassignment form). Reuse the existing assignment-target validation.
2. **Desugar**: rewrite to `place = <update>(place, index, value)` — type-directed
   to `dict.insert` / `list.set_at` (per "Stdlib" option 1). For a nested place
   `g[i][j] = v`, desugar outward:
   `g = g.set_at(i, g.at(i).set_at(j, v))` (note: this re-reads `g.at(i)`; fine for
   value semantics, and the uniqueness pass keeps the outer update in place).
3. **Both backends**: nothing new — the output is ordinary reassignment +
   `set_at`/`insert`, already supported and in-place-optimized.
4. **`fmt`**: print `place[index] = value` as the canonical form (and rewrite the
   verbose `d = d.insert(k, v)` self-assign to it? — left as a follow-up; fmt
   rewrites are the migration vehicle, but auto-rewriting reassignments is a
   bigger behavioral call).
5. **Parity test** + spec §4 (add a subscript-assignment row) and book updates.

## Alternatives considered

- **Status quo (`d = d.insert(k, v)`).** Works and is in-place, but the ceremony
  is exactly the friction this RFC removes.
- **`var self` mutating methods** (e.g. a `dict.insert(var self, k, v)` called as
  the bare statement `d.insert(k, v)`). witchy *does* support `var` write-back,
  including `var self` on user types, so this is real — but for the value-semantic
  stdlib collections it has three costs: (a) it compiles only when the receiver is
  a `var`, never a `let`, a literal, a temporary, or a chain link — all of which
  the functional `insert` accepts; (b) it must return `Nil`, which kills
  `d2 = d.insert(...)` and `.insert(...).insert(...)` chaining; (c) it buys no
  performance, since `d = d.insert(k, v)` is already an in-place O(1) update. So a
  `var self` variant would fork one operation into two APIs (and only one of them
  composes). `var self` remains the right tool for *user* aggregate types; for the
  builtins, the index-assignment desugar gives the same ergonomics without the
  fork. (Also: `dict.insert` / `list.set_at` are native-intercepted primitives, so
  their calling convention is not a free knob.)

## Drawbacks / open questions

- **Nested index-assignment** (`g[i][j] = v`) re-reads the outer container in the
  desugar; correct, but worth a test to confirm the uniqueness pass keeps it
  in place rather than copying.
- **Compound forms** (`d[k] += v`) are out of scope here; they could later desugar
  to `d[k] = d[k] <op> v` once a subscript read on `Dict` (`d[k]`) is also added
  (today only `list` has subscript read).
- Adds subscript-write to the parser's place grammar.

## Rollout

`proposed`. Sibling of [RFC-0021](./0021-or-unwrap-option.md) (the `||` Option
unwrap). Both are ergonomics over the value-semantic collections; land after the
compiler workspace refactor ([RFC-0018](./0018-compiler-architecture.md)) settles to avoid churn in the parser /
typeck / both lowerings.
