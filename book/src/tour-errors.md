# Errors as Values

witchy has no `null` and no exceptions. Two standard enums cover absence and
failure, and a single operator makes them ergonomic.

## `Option`: maybe a value

`Option(a)` is `Some(a)` or `None`. It's how a function says "I might not have an
answer" without inventing a sentinel like `-1` or `null`.

```witchy
fn first_even(xs: List(Int)) -> Option(Int):
    for x in xs:
        if x % 2 == 0:
            return Some(x)
    None

fn main(console: Console):
    match first_even([1, 3, 4, 7]):
        Some(n) -> print(console, "found " + "${n}")
        None -> print(console, "none")

    match first_even([1, 3, 5]):
        Some(n) -> print(console, "found " + "${n}")
        None -> print(console, "none")
```

```text
found 4
none
```

There's a shorthand for "do this only if it's `Some`":

```witchy
fn lookup(xs: List(Int), i: Int) -> Option(Int):
    if i < xs.length():
        Some(xs.at(i))
    else:
        None

fn main(console: Console):
    if let Some(v) = lookup([10, 20, 30], 1):
        print(console, "got " + "${v}")
    else:
        print(console, "out of range")
```

```text
got 20
```

And when you just want the value or a fallback, `??` unwraps an `Option`:
`Some(x) ?? d` is `x`, and `None ?? d` is `d` (with `d` evaluated only when there's
nothing to unwrap):

```witchy
fn lookup(xs: List(Int), i: Int) -> Option(Int):
    if i < xs.length():
        Some(xs.at(i))
    else:
        None

fn main(console: Console):
    print(console, "${lookup([10, 20, 30], 1) ?? 0}")
    print(console, "${lookup([10, 20, 30], 9) ?? 0}")
```

```text
20
0
```

`??` works on `Result` too — `parse(s) ?? 0` yields the `Ok` value or the
fallback, discarding the error (when the error matters, use `?`, `e? "msg"`, or
`match`). There is no truthiness in witchy: an empty string or list is data, not
falsehood — to default one, test it honestly
(`if name.is_empty(): "anon" else: name`).

## `Result`: a value or an error

`Result(a, e)` is `Ok(a)` or `Err(e)`. Use it when failure carries information —
*why* it failed:

```witchy
fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok: " + "${v}"
        Err(e) -> "error: " + e

fn main(console: Console):
    print(console, show(checked_div(10, 2)))
    print(console, show(checked_div(10, 0)))
```

```text
ok: 5
error: division by zero
```

## The `?` operator

Chaining fallible operations by hand — matching each result, propagating each
error — is tedious. The `?` operator does it: on `Ok`/`Some` it unwraps the
value and keeps going; on `Err`/`None` it returns that from the enclosing
function immediately.

```witchy
fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn average_of_ratios(a: Int, b: Int, c: Int) -> Result(Int, String):
    let first = checked_div(a, b)?
    let second = checked_div(first, c)?
    Ok(second)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok: " + "${v}"
        Err(e) -> "error: " + e

fn main(console: Console):
    print(console, show(average_of_ratios(100, 5, 2)))
    print(console, show(average_of_ratios(100, 0, 2)))
    print(console, show(average_of_ratios(100, 5, 0)))
```

```text
ok: 10
error: division by zero
error: division by zero
```

The happy path reads top-to-bottom like ordinary code, and every `?` is a
visible place where an error can leave the function. There's no hidden control
flow — no exception that unwinds through frames you can't see.

## When you *want* to crash

Sometimes a condition is a genuine bug, not an expected failure — and you want
to stop, loudly. `fail(message)` aborts with your message. So do the operations
that can't sensibly continue: indexing out of bounds, dividing by zero, parsing
nonsense as a number. These are **loud on every backend** — a runtime error in
the interpreter, a trap in the compiled VM — never a quietly wrong result. This
is the same parity discipline at work: failure is part of a program's observable
behavior, and witchy keeps it identical across backends.

```witchy
fn safe_sqrt_input(n: Int) -> Int:
    if n < 0:
        fail("expected a non-negative number")
    n

fn main(console: Console):
    print(console, "${safe_sqrt_input(9)}")

// safe_sqrt_input(0 - 1) would abort the program here.
```

```text
9
```

`fail` is a builtin — it needs no capability, because aborting isn't reaching
out to the world. It's the primitive the test framework's assertions are built
on, too (we'll meet `witchy test` later).

## Constructors that validate return `Result`

A function that *checks* its input before building a value hands back a
`Result`, not the bare value — so a bad input is a value you handle, not a
crash. Several standard constructors work exactly this way, and `?` threads
them cleanly:

```witchy
import time
import semver

fn parse_release(date: String, ver: String) -> Result(String, String):
    // Result(DateTime, String)
    let d = time.parse_iso8601(date)?
    // Result(Version, String)
    let v = semver.parse(ver)?
    Ok(time.date_string(d) + " v" + semver.format(v))

fn main(console: Console):
    match parse_release("2026-06-12T00:00:00Z", "1.4.0"):
        Ok(s) -> print(console, s)
        Err(e) -> print(console, "error: " + e)
```

```text
2026-06-12 v1.4.0
```

`time.civil(...)`, `time.parse_iso8601(...)`, `semver.parse(...)`, and
`url.parse(...)` all follow this shape: they verify the input — a real calendar
date, a well-formed version or URL — and report a bad one as `Err` instead of
guessing. So you can't accidentally use an unvalidated `DateTime`; the type
makes you unwrap the `Result` first.

Next, the tools for writing code that works for *many* types at once.
