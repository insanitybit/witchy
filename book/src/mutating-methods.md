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
    print(console, "${xs}")                      // [1, 2, 3]

    var tally = dict.new()
    tally.insert("a", 1)
    tally.insert("b", 2)
    print(console, "${tally.get_or("a", 0)}")    // 1
```

`xs.push(1)` as a statement is exactly `xs = list.push(xs, 1)` — the completion of
the `xs[i] = v` / `d[k] = v` family — and the uniqueness analysis keeps it an
in-place write, so a push loop stays O(n).

The rule is precise: a statement-position `place.method(args)` writes back **only
when** the place is a `var` (you cannot mutate a `let`) and `method` returns the
receiver's type. So `xs.push(v)` and `d.insert(k, v)` write back, while a query
like `xs.length()` stays a plain discard:

```witchy
fn main(console: Console):
    var xs = [1, 2, 3]
    xs.length()                  // a discard — `length` does not return a list
    print(console, "${xs}")      // [1, 2, 3], unchanged

    let frozen = [9, 9]
    // frozen.push(1) would be a compile error — `frozen` is a `let`
    print(console, "${frozen}")  // [9, 9]
```

In expression position the method call is unchanged — `let ys = xs.push(4)` builds
a new list and leaves `xs` alone — so only statements on a `var` place mutate.
