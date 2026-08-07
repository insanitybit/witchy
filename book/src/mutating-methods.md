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

The method call is the idiom; its module-qualified alias `list.push(xs, 3)`
resolves to the same `var`-declared operation. Statement position may discard a
`var` call's independent result because the write-back is already an effect. A
non-`var`, non-`()` result still requires a binding or `let _ =`.

An immutable binding or temporary cannot be a write-back target:

```witchy
fn main(console: Console):
    let frozen = [9, 9]
    // frozen.push(1)       // error: root must be `var`
    // [1].push(2)          // error: temporary has no write-back place
    console.print("${frozen}")
```

To derive a changed copy, make that copy explicit:

```witchy
fn sorted_copy() -> List(Int):
    let original = [3, 1, 2]
    var sorted = original
    sorted.sort()
    sorted

fn main(console: Console):
    console.print("${sorted_copy()}")
```

```text
[1, 2, 3]
```

This keeps one reading rule in expressions too. `let item = xs.pop()`, a call in
an argument, and a call inside `??` all commit write-back before the enclosing
expression continues.

## Returning the old value efficiently

Result-bearing mutators do not need a tuple convention or special call syntax.
The compiler carries the ordinary result and the final `var` value on separate
ABI channels:

```witchy
fn main(console: Console):
    var scores = dict.new()
    let first = scores.insert("ada", 36)   // None
    let old = scores.insert("ada", 37)     // Some(36)
    let removed = scores.remove("ada")     // Some(37)
    console.print("${old ?? 0} ${removed ?? 0}")
```

Those channels are one typed callable envelope. Direct calls, typed function
values, typed lambdas, and existential trait witnesses preserve the ordinary
result, every `var` write-back, and the associated collection ownership state.
A mutable record field or fixed-index element is also a valid write-back place:
the compiler captures its root and fixed path, stages the returned value, and
then rebuilds the root. This place rule preserves value semantics; a nested
collection still needs its own uniqueness proof before `mode opt` grants a
no-copy contract.

For `Dict.insert` and `Dict.remove`, one key search supplies both the returned
old value and the repair location. When the compiler proves the container has
one owner, it moves the old leaf out and repairs that storage directly;
`List.pop` is O(1) in the same case. If a live alias exists, normal mode copies
the container first so the alias keeps its old value. The behavior is identical;
only the ownership-dependent cost differs. In `mode opt`, these three receivers
carry a `unique` contract: an alias or active borrowed view is a compile error
with the ownership-loss reason instead of an implicit copy.
