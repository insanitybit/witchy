# Authority as a Value

## Capability values

A **capability** is a value whose type carries the authority to do something. You've
already met `Console`, which grants printing. There are a handful of these host
capabilities:

| Type | Grants |
|---|---|
| `Console` | writing to standard output |
| `Clock` | reading the wall clock |
| `Env` | reading environment variables |
| `Dir` | access to a directory subtree |
| `File` | access to a single file (the leaf of `Dir`) |
| `Net` | access to the network |
| `Exec` | spawning a native subprocess (the sharpest authority) |
| `Rand` | drawing cryptographically-secure randomness |
| `SecretStore` / `Secret` | named host secrets, and signing with one |

Each is *right-typed* where that matters - `Dir[Read]` vs `Dir[Write]`,
`Net[Connect, Tcp]` - so the type identifies the resource and the permitted
operations.

The host mints capabilities and hands them to `main`. Capability constructors
are unavailable to program code; the type checker enforces that boundary:

```witchy
fn main(console: Console, clock: Clock):
    // Milliseconds since the unix epoch; needs the Clock.
    let t = clock.now()
    console.print(if t > 0: "the clock is ticking" else: "epoch")
```

```text
the clock is ticking
```

`main`'s parameter list is the program's **root grant**. The host decides what
to put there; the program can't ask for more.

## Authority flows through values

Once you hold a capability it is an ordinary value, and you pass it as an
argument. There are no globals to stash it in and no ambient registry to fetch
it from, so a capability parameter tells you exactly where authority went.
Callbacks are values too. When you pass one, you hand over the right to call it
without handing over whatever capabilities its author captured - and the
receiver cannot reach those either.

```witchy
// `log` can print, because it was given a Console. `compute` cannot, because it
// wasn't - and there's nowhere for it to get one.
fn compute(x: Int) -> Int:
    x * x + 1

fn log(console: Console, label: String, value: Int):
    console.print("${label}: ${value}")

fn main(console: Console):
    let result = compute(6)
    log(console, "compute(6)", result)
```

```text
compute(6): 37
```

`compute` is effect-free as written: it takes ordinary data and does
arithmetic. Its signature proves it holds no capability of its own - but read
that narrowly. "No capability parameter" is not the same as "no effects": a
function that takes a callback can call it, and that callback may do whatever
its author's captured capabilities allow, without this function ever seeing
them. When you want an effect-free promise you can rely on, write `pure fn`,
which the compiler checks. A plain `fn` makes no such promise.

## Inspecting authority with `witchy caps`

Because authority is visible in types, `witchy caps` can compute a program's
footprint:

```sh
witchy caps program.witchy
```

For a program whose `main` takes `Console` and `Dir[Read]`, it prints which
functions need which capabilities, plus the total. It works this out *from the
source* rather than from anything you declare, and the package manager runs the
same computation over your dependencies.

## Effects are explicit inputs

Reading the clock needs a `Clock`, because the current time comes from outside
the program. Environment variables (`Env`) and randomness work the same way. If
a function reads one of these directly, the capability is in its signature.
It can also take a narrower operation as a callback instead - calling that
exercises authority someone already delegated, not a new grant.

You can't conjure a `Clock` from an integer, and there's no global to reach for.
If a function has authority, someone handed it over.
