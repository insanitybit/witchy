# Error-Handling Patterns

witchy has no exceptions. A function that can fail returns a `Result(ok, err)`,
and the `?` operator threads failures for you. The tour covered the basics; this
chapter is the working playbook - a custom error set, `?` propagation, and the
`result`/`option` combinators that replace a pile of `match` blocks.

## Propagating with `?` and an anonymous error set

You rarely need to name an `enum` for a local error type. An **anonymous union** -
`type Name = .[Variant | Variant(payload)]` - gives you a closed, matchable
error set without the ceremony, and `?` propagates it: on `Err` it returns
early, on `Ok` it unwraps.

```witchy
type ParseError = .[Empty | NotANumber(String)]

fn parse_positive(s: String) -> Result(Int, ParseError):
    if s == "":
        return Err(.Empty)
    match string.parse_int(s):
        Some(n) -> Ok(n)
        None -> Err(.NotANumber(s))

fn sum_all(inputs: List(String)) -> Result(Int, ParseError):
    var total = 0
    for s in inputs:
        // `?` returns early on Err, unwraps on Ok.
        let n = parse_positive(s)?
        total = total + n
    Ok(total)

fn show(r: Result(Int, ParseError)) -> String:
    match r:
        Ok(n) -> "total ${n}"
        Err(.Empty) -> "error: empty input"
        Err(.NotANumber(s)) -> "error: '${s}' is not a number"

fn main(console: Console):
    console.print(show(sum_all(["1", "2", "3"])))
    console.print(show(sum_all(["1", "x", "3"])))
    console.print(show(sum_all(["1", "", "3"])))
```

```text
total 6
error: 'x' is not a number
error: empty input
```

The `sum_all` loop reads like straight-line code - no error plumbing between the
steps - yet a single bad input short-circuits the whole thing with a precise
error. A smaller error set also *widens* into a larger one through `?`, so
helpers compose without wrapper code (see the errors chapter of the tour for the
widening rules).

## Combinators instead of `match`

When you only want a default, a transform, or a chained step, a combinator is
shorter and clearer than a `match`:

```witchy
fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("divide by zero")
    else:
        Ok(a / b)

fn main(console: Console):
    // Combinators avoid a match when you just want a default or a transform.
    console.print("${checked_div(10, 2).unwrap_or(-1)}")
    console.print("${checked_div(10, 0).unwrap_or(-1)}")
    // map_ok transforms the Ok payload; and_then chains a second fallible step.
    let doubled = checked_div(20, 4).map_ok(fn(n: Int): n * 2)
    console.print("doubled: ${doubled.unwrap_or(0)}")
    // result.all collects a list of Results into a Result of a list — first Err wins.
    let parts = [checked_div(8, 2), checked_div(9, 3), checked_div(5, 0)]
    match result.all(parts):
        Ok(xs) -> console.print("all ok: ${list.sum(xs)}")
        Err(e) -> console.print("stopped at: ${e}")
```

```text
5
-1
doubled: 10
stopped at: divide by zero
```

The workhorses:

- `unwrap_or` / `unwrap_or_else` - supply a fallback for `Err`.
- `map_ok` / `map_err` - transform one side, leave the other untouched.
- `and_then` - chain a second operation that itself returns a `Result`.
- `or_else` - recover by trying an alternative.
- `result.all` - turn a `List(Result)` into a `Result(List)`, failing on the
  first `Err`; `result.partition` keeps both the oks and the errs instead.

`Option` has the parallel set (`unwrap_or`, `map`, `and_then`, `filter`,
`ok_or`), and `ok_or` bridges an `Option` into a `Result` when a missing value
should become a real error. Reach for a combinator when the shape is "default /
transform / chain"; reach for `match` when you genuinely need to branch on every
variant, as `show` does above.
