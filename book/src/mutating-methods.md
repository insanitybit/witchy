# Uniform `var` write-back

A `var` parameter means move-in/move-out in every call. Its argument must be a
mutable place; the function receives the current value and writes its final value
back on every structured return. The ordinary return value is independent.

```witchy
fn take_last(var xs: List(Int)) -> Option(Int):
    xs.pop()

fn main(console: Console):
    var xs = [1, 2]
    xs.push(3)
    let last = take_last(xs)
    console.print("${xs}")
    console.print("${last ?? 0}")

    var tally = dict.new()
    tally.insert("a", 1)
    tally.insert("b", 2)
    console.print("${tally.get_or("a", 0)}")
```

Free and method calls are equivalent: `xs.push(3)` resolves to the same
`var`-declared operation as `list.push(xs, 3)`. Statement position may discard a
`var` call's independent result because the write-back is already an effect. A
non-`var`, non-`Nil` result still requires a binding or `let _ =`.

An immutable binding or temporary cannot be a write-back target:

```witchy
fn main(console: Console):
    let frozen = [9, 9]
    // frozen.push(1)       // error: root must be `var`
    // list.push([1], 2)    // error: temporary has no write-back place
    console.print("${frozen}")
```

To derive a changed copy, make that copy explicit:

```witchy
fn sorted_copy() -> List(Int):
    let original = [3, 1, 2]
    var sorted = original
    sorted.sort()
    sorted
```

This keeps one reading rule in expressions too. `let item = xs.pop()`, a call in
an argument, and a call inside `??` all commit write-back before the enclosing
expression continues.
