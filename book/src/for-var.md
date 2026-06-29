# Mutating elements in a loop — `for var`

`for var x in xs:` binds each element of a list **mutably** and writes it back, so
you update elements in a loop without index bookkeeping. It is the loop form of
mutable value semantics: the mutation lands in `xs` — in place, when the
uniqueness analysis can prove `xs` is unaliased, so it stays O(n).

```witchy
type Account:
    name: String
    balance: Int

fn main(console: Console):
    var xs = [1, 2, 3, 4]
    for var x in xs:
        x = x * 10
    print(console, "${xs}")                        // [10, 20, 30, 40]

    var accounts = [Account("ada", 100), Account("bob", 50)]
    for var a in accounts:
        a.balance = a.balance + 25
    print(console, "${accounts.at(0).balance}")    // 125
    print(console, "${accounts.at(1).balance}")    // 75
```

A plain `for x in xs:` binds each element read-only — assigning to `x` there is a
compile error. Reach for `for var` only when you intend to update elements.

In this first version the body must run straight through: a `break`, `continue`,
`return`, or `?` that belongs to the loop is a compile error, because it would
skip the write-back of the current element. Use an index loop
(`for i in 0..xs.length(): ...`) if you need an early exit. Loss-free write-back
across early exit is a planned refinement (RFC-0028).
