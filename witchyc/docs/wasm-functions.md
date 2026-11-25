# Functions and their result

```
fn larger(a: U64, b: U64) -> U64:
    if a > b:
        a
    else:
        b
```

A function with a result gives it with `return value`, or by ending in the value: a last
expression is the result, and a last `if`/`else` or `match` passes that on to each of its
branches' last statements (`using` and blocks likewise). Closures with a block body follow the
same rule. A body whose last statement is not the result's type is refused there
(`functions/ends-in-wrong.refused.wy`); one that can reach its end without a value is refused
as falling through. Contracts and proofs read the ending value as its `return`
(`functions/ends-in-value.wy`).

## Labelled arguments

```
greet(n: 3, by: 4)
greet(5, by: 2)
Account(name: 7, balance: 100)
```

An argument may name the parameter it is given to (or, making a record, its field), and the
name is checked (`functions/labels.wy`, `labels-wrong.refused.wy`). Labels that follow the
parameters' order may label any argument. Out of that order every argument must be a name, a
field of one, a literal or a reference to one of those, so that the order in which arguments
are evaluated, which is the order they are written, cannot matter
(`labels-effect.refused.wy`). Labels name parameters of a function called by its name and
fields of a record being made; method calls and closures take none yet.

## Default arguments

```
fn scale(n: U64, by: U64 = 10, plus: U64 = 0) -> U64:
scale(4)                  // scale(4, 10, 0)
scale(4, 2)               // scale(4, 2, 0)
scale(n, plus: 1)         // scale(n, 10, 1)
```

A parameter may have a default: a number or Bool written out (a negative number too), checked
against the parameter's type like any literal. Parameters with a default come after all those
without one. A call leaves trailing ones out, or skips some by labelling a later parameter; the
arguments it writes are still evaluated in the order written, and a default, being a literal,
runs nothing (`functions/defaults*.wy`). Defaults are filled for functions called by their
name; a method's parameters take none yet.

## Record spread

```
let big = Style(..base, size: 20)
```

`..base` in making a record gives every field not written from `base`, which is a name, a
field of one, or an element at a name or a number (`..items[1]`; it is read once per field); the fields written are labelled, and out of the
record's order only when they are names, fields or literals, as for labelled arguments
(`fields/spread.wy`). A field holding an owner cannot be copied out of `base`, so spreading a
record that holds one is refused (`fields/spread-owner.refused.wy`). A written field may read
the base (`Stats(..stats, words: stats.words + 1)`, `fields/spread-reads-base.wy`) but not
change it: no `&mut` of it, `move` of it or method call on it, so where the base's fields are
read among the written values cannot matter (`fields/spread-uses-base.refused.wy`). The base's fields are taken by name, so a base of
another record type with the same field names is accepted.

## Lines inside brackets

While a `(`, `[` or `{` is open the line goes on, so arguments, parameters, arrays and records
may be written one to a line, indented as one likes, with a trailing comma
(`functions/multiline-brackets.wy`). Indentation counts only on lines that begin outside every
bracket.
