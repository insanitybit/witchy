# Tuples

```
fn divide(n: U64, d: U64) -> (U64, U64):
    (n / d, n % d)

let (q, r) = divide(17, 5)
let t = divide(9, 2)
console.print_int(t.0 * 10 + t.1)
var pairs: [(U64, U64); 2] = [(1, 2), (3, 4)]
let Ok(pair): (String, U64) = labelled(&allocator, 3) else Err(code):
    return
let (text, n) = move pair
```

A tuple is the anonymous record of its values by position: `(U64, Bool)` is `.{0: U64, 1:
Bool}`, `(a, b)` is `.{0: a, 1: b}`, and `t.0` reads its first value, the same record a
variant's `.Tag(a, b)` payload is. So a tuple is copied when all its values are, holds owners
like any record (moved with `move`, opened by `let (a, b) = move pair`), and may be an array or
`Vec` element, a parameter, a result and a type argument, with no rule of its own
(`tuples/basic.wy`, `tuples/owner.wy`).

- Two to eight values; `(T)` is `T`, and `()` is the unit.
- `var (a, b) = pair` binds variables, and either form may state the tuple's type:
  `var (count, total): (U64, U64) = (0, 0)` (`tuples/var.wy`).
- `let (a, _) = pair` takes nothing from `_`'s place, which may not hold an owner
  (`tuples/drop-owner.refused.wy`).
- `t.0.1` reads as the decimal number `0.1`: write `(t.0).1`.
