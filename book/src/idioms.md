# Idiomatic witchy

Witchy usually offers exactly one blessed way to say a thing. This chapter is
the house style in one place: each section shows the idiom next to the
spelling it replaces. Both versions run — the unidiomatic forms are legal,
just worse — and every example here is executed by the test suite.

## Interpolation, not concatenation

`"${expr}"` renders any value. Reach for `+` only when byte-exact joining is
the point.

```witchy
fn main(console: Console):
    let host = "example.com"
    let port = 8080
    // Idiomatic:
    console.print("dialing ${host}:${port}")
    // Not: "dialing " + host + ":" + "${port}"
    console.print("dialing " + host + ":" + "${port}")
```

```text
dialing example.com:8080
dialing example.com:8080
```

## Uniform `var` write-back

A `var` parameter writes back in every expression position. Method and free
forms are equivalent: `xs.push(v)` and `list.push(xs, v)` both require a mutable
place. The same rule covers `d.insert(k, v)`, `d[k] = v`, and `xs[i] = v`.

```witchy
fn main(console: Console):
    var out = []
    for n in 1..4:
        out.push(n * n)        // idiomatic — not: out = list.push(out, n * n)
    var ages = dict.new()
    ages.insert("ada", 36)     // idiomatic — not: ages = dict.insert(ages, ...)
    ages["bob"] = 41           // index sugar over insert
    console.print("${out} ${ages.length()}")
```

```text
[1, 4, 9] 2
```

## `Result` + `?` for failure — never `Option(String)`-as-error

Absence is `Option` (the reason doesn't matter); failure is
`Result(T, String)` (it does). Propagate with `e?`, add context with
`e? "msg"`. **Do not** encode failure as `Option(String)` with `Some`
meaning "error" — the polarity is inverted and `?` can't compose with it.

```witchy
import result

fn parse_port(s: String) -> Result(Int, String):
    match string.parse_int(s):
        Some(n) -> if n > 0 && n < 65536: Ok(n) else: Err("port ${n} out of range")
        None -> Err("`${s}` is not a number")

fn endpoint(host: String, port_s: String) -> Result(String, String):
    let port = parse_port(port_s)? "bad endpoint"
    Ok("${host}:${port}")

fn main(console: Console):
    match endpoint("example.com", "8080"):
        Ok(e) -> console.print(e)
        Err(m) -> console.print(m)
    match endpoint("example.com", "banana"):
        Ok(e) -> console.print(e)
        Err(m) -> console.print(m)
```

```text
example.com:8080
bad endpoint: `banana` is not a number
```

## Combinators and comprehensions, not index loops

`while i < n` with manual indexing is the spelling of last resort. Prefer a
comprehension for map/filter shapes, `for` for iteration, and the
`list`/`iter` combinators (`fold`, `any`, `all`, `find`) for the rest —
lazily via `iter` when the pipeline is long or the source is large.

```witchy
fn main(console: Console):
    let xs = [3, 1, 4, 1, 5, 9]
    console.print("${[n * n for n in xs if n % 2 == 1]}")
    console.print("${xs.any(fn(n: Int): n > 8)}")
    console.print("${xs.fold(0, fn(acc: Int, n: Int): acc + n)}")
```

```text
[9, 1, 1, 25, 81]
true
23
```

## Use the stdlib; don't hand-roll beside it

Before writing a helper, check [the stdlib](appendix-stdlib.md) — the gap you
are papering over is usually already covered (`json.get_string`,
`string.split_once_opt`, `list.contains`, `dict.get_or`, …). A private
wrapper around a std function is a smell: either the std function already
does what you want, or the gap is worth filing, not wrapping.

```witchy
import json

fn main(console: Console):
    let doc = json.decode("{\"name\": \"ada\", \"level\": 3}") ?? json.object_sorted([])
    // Idiomatic: the accessor family — not a hand-rolled match ladder.
    console.print(json.get_string(doc, "name") ?? "anonymous")
    console.print("${json.get_int(doc, "level") ?? 0}")
```

```text
ada
3
```

## Methods on capabilities, like any other value

Capability operations are spelled as methods on the capability
([RFC-0076](../rfcs/0076-capability-ops-are-methods.md)): `console.print(s)`,
`dir.read(path)`, `net.connect(addr)`. The receiver *is* the authority — the
spelling keeps that visible.

## Sealed types for invariants; sealed capabilities for policy

When data carries a rule ("0–100", "a real date", "distinct members"), seal
the type and let one smart constructor own the rule — a value of the type is
then proof the rule holds. See [Data: Records and Enums](tour-data.md) for
the mechanics, and [Capabilities](capabilities.md) for the same move applied
to authority (glamour's `UiFetch` tokens are policy you cannot forge).

## The shape of a witchy program

Push effects to the edges. `main` receives capabilities and delegates;
the middle of the program is pure functions over data (easy to test — no
capabilities to fake); the leaves take exactly the capability they use,
narrowed as far as it will go (`Dir[Read]`, not `Dir`). If a function's
signature has no capabilities, it provably has no effects — keep as much of
the program in that state as you can.
