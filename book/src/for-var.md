# Mutating elements in a loop - `for var`

Updating a list in place usually costs you one of two things. Either you write
index bookkeeping and hope you got the bounds right, or the language hands you a
reference into the list and you inherit aliasing along with it. `for var x in
xs:` does neither.

It binds each element **mutably** and writes it back, so the mutation lands in
`xs` - in place, when the uniqueness analysis can prove `xs` is unaliased, so it
stays O(n). It's the loop form of mutable value semantics.

```witchy
type Account:
    name: String
    balance: Int

fn main(console: Console):
    var xs = [1, 2, 3, 4]
    for var x in xs:
        x = x * 10
    // [10, 20, 30, 40]
    console.print("${xs}")

    var accounts = [Account("ada", 100), Account("bob", 50)]
    for var a in accounts:
        a.balance += 25
    // 125
    console.print("${accounts.at(0).balance}")
    // 75
    console.print("${accounts.at(1).balance}")
```

A plain `for x in xs:` binds each element read-only - assigning to `x` there's a
compile error. Reach for `for var` only when you intend to update elements.

In this first version the body must run straight through: a `break`, `continue`,
`return`, or `?` that belongs to the loop is a compile error, because it would
skip the write-back of the current element. Use an index loop
(`for i in 0..xs.length(): ...`) if you need an early exit.
