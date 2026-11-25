# `match` and `if` as values

```
let area = match move shape:
    Dot:
        0
    Box(width, height):
        if width == height:
            width * width
        else:
            width * height
```

`let name = match ...:` and `let name = if ...: ... else: ...` (also `var`, also
with a written type) bind the value that ends whichever path runs. A path may
instead leave with `return`, `break` or `continue`. `else if` chains are accepted
everywhere an `else` is.

The same forms follow `return`, `=`, `+=`, `-=`, `*=`, `/=` and `%=`:

```
return if left < right:
    0
else:
    1

total += match found:
    Ok(value):
        value
    Err(code):
        0
```

There each path's value becomes that `return` or assignment, written once per
path, so the target of an assignment must be a name, a field or an element at a
named index. No checker rule is involved.

## How it works

The parser turns the value that ends each path into a hidden `Yield` statement
and refuses a path that ends in anything else (`src/parser.rs::tails`). So every
path that reaches the statement after the `match` has been through one `Yield`;
that is a property of the syntax tree, not of a later analysis. The checker
(`src/compiler/check/statements.rs`, `Choose` and `Yield`) makes one local on the first `Yield`,
with that value's type or the written one, and holds every later `Yield` to it.
An owner is live only from its `Yield`; after the statement it is an ordinary
local: it must be consumed once, and cannot be consumed twice.

A written type also settles a generic call being matched: in
`var out: Vec<Token> = match Vec.new(allocator, 0): Ok(empty): move empty ...`
the arm hands `Ok`'s value to `out`, so `Ok` holds a `Vec<Token>`. The hint is
used only for type parameters the arguments leave open, so it cannot change a
call that was already accepted.

## Refused

- an `if` without `else`; a path that ends in a statement that is not a value
  and does not leave; a path whose value is of another type or is `()`;
- a reference as the chosen value;
- inside a function whose contract speaks of memory (the memory and record models
  do not read it yet).

## In proofs

A function with a contract about numbers may use it: each path's value is an
assignment the proof walk already follows, and the paths join like the arms of any
`match`. `tests/fixtures/choose/proved.wy` proves a maximum, a clamp and a count
through a nested choice; `false-claim.refused.wy` is refused.

Fixtures: `tests/fixtures/choose/`, `tests/fixtures/programs/calc.wy`.
Nothing new is trusted: the emitted code is local assignments inside the
existing `match` and `if`.

## `if c: a else: b` on one line

```
return if a > b: a else: b
console.print_int(if wrote: 1 else: 0)
```

A one-line `if` with `else` is an expression. It becomes a `Choose` of a hidden
name just before the statement that holds it, so only the chosen side runs and
proofs follow it like any chosen value (`conditional/proved.wy`). Moving it ahead
of the statement must not change what runs first, so whatever is written before it in
the statement and does something (a call, arithmetic that may trap) is first bound to a
hidden name, in order (`conditional/after-call.wy`, `call-after-arithmetic.traps.wy`,
`read-then-move.wy`). On the right of `and`/`or`, which may not run, it is refused, as
`?` is. `let x = if c:` with its paths on
lines below stays the block form. Fixtures: `tests/fixtures/conditional/`.
