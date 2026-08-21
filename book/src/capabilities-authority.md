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

A capability is an ordinary value once you've it, and code passes it as an
argument. There are no globals to stash it in and no ambient registry to fetch
it from. Capability-typed parameters make directly possessed authority visible
and local. Ordinary callbacks are values too: passing one delegates the right
to invoke its interface, while the capabilities captured by its creator remain
opaque to the receiver.

```witchy
// `log` can print, because it was given a Console. `compute` cannot, because it
// wasn't — and there's nowhere for it to get one.
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

`compute` is effect-free as written: it receives ordinary data and performs
arithmetic. Its signature proves that it possesses no direct host capability,
but lack of a capability parameter is not a general purity contract. A function
that accepts an ordinary callback may invoke effectful behavior deliberately
delegated by its caller, without learning or possessing the callback's captured
capabilities. The `pure fn` qualifier makes effect-free invocation an explicit
checked API promise; ordinary `fn` remains opaque and potentially
effectful.

## Inspecting authority with `witchy caps`

Because authority is visible in types, `witchy caps` can compute a program's
footprint:

```sh
witchy caps program.witchy
```

For a program whose `main` takes `Console` and `Dir[Read]`, it prints which
functions demand which capabilities, and the total. This is computed
*from the source*: the tool re-derives what each function requires. The package
manager checks that computed footprint for dependencies.

## Effects are explicit inputs

Reading the clock needs a `Clock`: current time is an input from outside the
program. The same applies to environment variables (`Env`) and randomness. A
function that reads one of these inputs directly lists the corresponding
capability in its signature. It may instead receive a narrower operation as an
ordinary callback; invoking that operation exercises delegated behavior, not a
new or ambient root grant.

You can't conjure a `Clock` from an integer, and there's no global to reach
for. If a function has authority, someone handed it over.
