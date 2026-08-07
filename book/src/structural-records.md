# Structural Records and Width Conformance

Anonymous records are exact structural types. They are useful when the shape is
the contract and a nominal constructor would add ceremony without adding an
invariant.

```witchy
type Value(a) = .{value: a}
type Located(a) = .{..Value(a), line: Int, label: String}

fn describe(row: .{label: String, value: String, line: Int}) -> String:
    "${row.line}:${row.label}:${row.value}"

fn main(console: Console):
    let row: Located(String) = .{label: "ready", value: "payload", line: 7}
    console.print(describe(row))
    console.print("${row}")
```

```text
7:ready:payload
.{label: ready, line: 7, value: payload}
```

Field order does not affect identity. The type
`.{label: String, value: String, line: Int}` above is exactly the same type as
`Located(String)` after its aliases are resolved.

## Compose a new exact shape

The type spread `.{..Base, extra: Type}` resolves `Base`, copies its fields, and
then produces one ordinary exact anonymous-record type. Bases may be aliases,
qualified aliases from another module, or generic aliases after substitution.
Composition is not a runtime wrapper and does not create a nominal subtype.

- Repeating a field with the identical type collapses to one field.
- Repeating it with a different type is an error.
- A cyclic base, unresolved base, nominal record, tuple, union, or non-record
  base is an error.
- Structural shapes remain capability-safe: a projection cannot hide authority
  in a field that the structural-type rules would reject.

Type spread and value spread are separate operations:

```text
type Detailed = .{..Summary, note: String}  # compose a type
let renamed = .{label: "new", ..detailed}  # update an exact value
```

Value spread preserves the base value's exact shape. It does not add or remove
fields.

## Richer values at expected-type sites

When an expression has a richer anonymous-record type and the surrounding
source explicitly expects a poorer anonymous-record type, Witchy constructs the
exact target shape. Every target field must exist at exactly the required type.

```witchy
type Public = .{id: Int, label: String}
type Private = .{..Public, secret: String}

type Envelope:
    row: Public

fn accept(row: Public) -> String:
    "${row}"

fn returned(row: Private) -> Public:
    row

fn main(console: Console):
    let private: Private = .{id: 7, label: "ready", secret: "kept"}
    let annotated: Public = private
    let rows: List(Public) = [private]
    let envelope = Envelope(private)

    console.print(accept(private))
    console.print("${annotated}")
    console.print("${list.at(rows, 0)}")
    console.print("${envelope.row}")
    console.print("${returned(private)}")
    console.print("${private}")
```

```text
.{id: 7, label: ready}
.{id: 7, label: ready}
.{id: 7, label: ready}
.{id: 7, label: ready}
.{id: 7, label: ready}
.{id: 7, label: ready, secret: kept}
```

The expected-type sites are:

| Site | Example |
|---|---|
| annotation or assignment | `let public: Public = private` |
| function argument | `accept(private)` |
| function return or tail | a `Private` expression in a function returning `Public` |
| typed aggregate slot | `List(Public)`, tuples, and declared record fields |
| explicit conversion | `private as Public` |
| default, `let`, or `own` parameter | the callee's declared parameter type |

The source expression is evaluated once. Projection reads its fields in the
target's declared order and constructs one exact target value before either
backend executes it.

## Projection changes the observable value

Extra fields are removed, not hidden. Rendering, JSON, reflection, equality,
hashing, dictionary keys, and runtime type information all see only the target
shape.

```witchy
import json

type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, revision: Int}

fn main(console: Console):
    let detailed: Detailed = .{id: 7, label: "ready", revision: 3}
    let summary: Summary = detailed
    let expected: Summary = .{id: 7, label: "ready"}

    console.print("${summary}")
    console.print(json.stringify(summary))
    console.print("${summary == expected}")
    console.print("${detailed}")
```

```text
.{id: 7, label: ready}
{"id":7,"label":"ready"}
true
.{id: 7, label: ready, revision: 3}
```

Strings, containers, closures, and other reference-bearing fields retain their
ordinary typed representation. Projection is a checked construction; it is not
a layout cast, a nominal relabel, or a conversion through an untyped slot.

## Inference stays exact

Width conformance is directed only by an explicit expected type. Witchy does
not guess a common smaller shape for an unannotated branch, container, function
value, or generic argument. This keeps inference local and avoids an extra
field from disappearing merely because another expression lacks it.

The following partial examples are intentionally rejected:

```text
# No expected element type: the two exact element types differ.
let rows = [.{id: 1, label: "a"}, .{id: 2}]

# No annotated join type: branches keep their exact types.
let row = if choose:
    .{id: 1, label: "a"}
else:
    .{id: 2}

# A nominal record with the same fields is still nominal.
type User:
    id: Int
let public: .{id: Int} = User(1)
```

Add the intended target where the narrowing is deliberate - for example,
`let rows: List(.{id: Int}) = [...]` or
`let row: .{id: Int} = if ...`.

## `let`, `own`, and `var`

A normal borrowed argument and a `let` argument project a temporary exact
target while leaving the richer source unchanged. An `own` argument consumes
the source binding exactly as any other owning call does.

```witchy
type Public = .{id: Int, label: String}
type Private = .{..Public, secret: String}

fn borrow(row: Public) -> String:
    "borrow ${row.label}"

fn hold(let row: Public) -> String:
    "let ${row.label}"

fn consume(own row: Public) -> String:
    "own ${row.label}"

fn main(console: Console):
    let private: Private = .{id: 7, label: "ready", secret: "kept"}
    console.print(borrow(private))
    console.print(hold(private))
    console.print("${private}")

    let moved: Private = .{id: 8, label: "done", secret: "gone"}
    console.print(consume(move moved))
```

```text
borrow ready
let ready
.{id: 7, label: ready, secret: kept}
own done
```

Using `moved` afterward is a use-after-move error. A `var Public` parameter is
different: it may replace its argument with any `Public`, so a `Private` caller
place could not regain the omitted `secret` during write-back. Witchy therefore
rejects `var Private -> var Public` for direct bindings, fields, indexes, and
nested places before reserving or mutating the caller place.

## Migrating manual projections

Code written before width conformance often copied fields by hand:

```text
fn public(row: Private) -> Public:
    .{id: row.id, label: row.label}
```

Keep a helper when it validates, renames, computes, or redacts values. If it
only selects fields with the same names and types, return the richer expression
directly or use `row as Public`. The checked projection has the same exact
observable target shape and is shared by the interpreter and compiled Wasm.

## Prove projection cost in the browser

In a runnable browser cell, `mode opt` displays deterministic compiled-resource
counters below the program output. These are operation counts, not timings.
Run the following program with `1`, then change it to `64`. The extra 63
projections add at most 63 `rc_alloc_calls` and 63 `bump_alloc_calls`, and
exactly 63 closed-loop `region_rewind_calls`. The output also proves that the
richer source remains intact.

```witchy
mode opt

type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, revision: Int}

fn main(console: Console):
    let row: Detailed = .{id: 7, label: "ready", revision: 3}
    var i = 0
    var total = 0
    while i < 1:
        let summary: Summary = row
        total = total + summary.id
        i = i + 1
    console.print("${total} ${row.revision}")
```

```text
7 3
```

The current baseline constructs one exact target record per projection. A
future scalar-replacement optimization may reduce these counts without changing
the documented upper bound or observable behavior.
