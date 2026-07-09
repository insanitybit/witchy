# Mutating-method statements — `nodes.push(x)`

A method call used as a **statement** on a mutable place writes its result back to
that place, so the data libraries read like in-place mutation while keeping value
semantics underneath:

```witchy
fn main(console: Console):
    var xs = []
    xs.push(1)
    xs.push(2)
    xs.push(3)
    // [1, 2, 3]
    console.print("${xs}")

    var tally = dict.new()
    tally.insert("a", 1)
    tally.insert("b", 2)
    // 1
    console.print("${tally.get_or("a", 0)}")
```

`xs.push(1)` as a statement is exactly `xs = list.push(xs, 1)` — the completion of
the `xs[i] = v` / `d[k] = v` family — and the uniqueness analysis keeps it an
in-place write, so a push loop stays O(n).

The rule is precise, and it is a **declaration**: a statement-position
`place.method(args)` writes back **only when** the place is a `var` (you cannot
mutate a `let`) and the resolved function declares a `var` receiver — that is
what marks it a mutator (`fn push(var xs: List(a), x: a) -> List(a)`). So
`xs.push(v)` and `d.insert(k, v)` write back, while a call that is *not* a
mutator and whose result is thrown away is a **compile error** — you either
bind it, reassign it, or discard it explicitly with `let _ =`:

```witchy
fn main(console: Console):
    var xs = [1, 2, 3]
    // Explicit discard: `length` is not a mutator.
    let _ = xs.length()
    // [1, 2, 3], unchanged
    console.print("${xs}")

    let frozen = [9, 9]
    // frozen.push(1) would be a compile error — `frozen` is a `let`
    // [9, 9]
    console.print("${frozen}")
```

Writing `xs.length()` as a bare statement is now that error (`result of
`length` is discarded`): it catches the mistake of calling a value-returning
method and forgetting to use its result. In expression position the method call
is unchanged — `let ys = xs.push(4)` builds a new list and leaves `xs` alone —
so only statements on a `var` place mutate.
