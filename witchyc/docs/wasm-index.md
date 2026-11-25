# `items[i]`

```
fn total(items: &Vec<U64>) -> U64:
    var sum: U64 = 0
    for i in 0..items.len():
        sum += items[i]
    return sum
```

`value[at]` on anything but a fixed array is the call `value.index(at)`: the
type's own method says what reading at a position is. Fixed arrays keep their
built-in indexing. This is Witchy 1's checked indexing (rfcs/0177, section 2):
callers write `items[index]`, and an index out of range is a defined failure,
not an assumption.

`Vec` defines it in library code, for elements that are copied:

```
fn index(vector: &Vec<T>, index: U64) -> T:
    match vector.get(index):
        Ok(item):
            return item
        Err(code):
            unreachable
```

So `items[3]` on a three-element Vec stops the run with a trap
(`tests/fixtures/index/past-end.traps.wy`). `get` remains the call that asks and
answers with a `Result`. A type without an `index` method is refused
(`no-index.refused.wy`).

Assignment is the same: `items[i] = v` is `items.index_set(i, v)`, and
`items[i] += v` (or `-=`) is `items.index_set(i, items[i] + v)`. Since that reads
the element before writing it, the value and the position must be names,
fields or literals, as for `*=` (`assign-busy.refused.wy`). `Vec.index_set`
traps past the end (`assign-past-end.traps.wy`); `tests/fixtures/index/assign.wy`
counts digits in a Vec. `&items[i]` is not rewritten; `at` and `at_mut` give
references.

An element's fields read and write the same way: `cells[i].pos.x` is a field of
`cells.index(i)`, and `cells[0].count += 10` binds the position, then the value, in the
order written, and writes the field through `cells.at_mut(0)`, trapping past the end as
`index_set` does. The Vec is a name or a field of one, so naming it runs nothing; the value is
evaluated before the element is borrowed, so it may read the same Vec
(`cells[1].pos.x = cells[0].count + 1`) (`index/element-fields.wy`, `element-field-order.wy`,
`element-field-past-end.traps.wy`).

## Cost

The inliner now runs up to three rounds, so a function whose calls were all
written in becomes a candidate itself: `items[i]` calls `Vec.index`, which calls
`Vec.get`, and both end as code in the loop. 2e7 reads, unmetered, best of
three: `get` with a `match` 0.052 s, `items[i]` 0.051 to 0.056 s (0.068 s with
one round). The rounds also helped String byte scans (0.092 s to 0.076 s) and
`Vec.set` (0.068 s to 0.056 s). `src/compiler/inline.rs` has a unit test for a
call chain written in over two rounds.

## Trusted

The rewrite is in the type checker and produces an ordinary method call, which
is checked like any other. `Vec.index` is library code with no proof; its read
goes through `get`, which is proved. Nothing new is trusted.

`tests/fixtures/programs/diff.wy` reads its table with `table[i]`.
