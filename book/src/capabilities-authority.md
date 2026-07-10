# Authority as a Value

## A capability is a token you cannot forge in code

A **capability** is a value whose type grants permission to do something. You've
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

Each is *right-typed* where that matters — `Dir[Read]` vs `Dir[Write]`,
`Net[Connect, Tcp]` — so the type says not just *which* resource but *what you may
do* with it. The next chapter is all about that narrowing.

The defining property: **you cannot construct one.** There is no `Console()`,
no `Dir.read_file("/")`, no global `stdin`. The type checker knows these types have
no constructor available to your code. The *only* capabilities in a program are
the ones the host mints and hands to `main`:

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
to put there; the program cannot ask for more.

## Authority flows only by argument

A capability is an ordinary value once you have it, so it moves the only way
values move: by being passed as an argument. There are no globals to stash it
in, no ambient registry to fetch it from. That means a function's authority is
*exactly* its capability-typed parameters — visible, local, complete.

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

Read `compute`'s signature: `fn compute(x: Int) -> Int`. No capabilities. It is
*provably* incapable of any effect — it can only compute. You don't have to read
its body, or its callees' bodies, to know that. Contrast a typical language,
where `compute` could be doing anything, and the only way to find out is to
audit the entire call graph.

## What this buys you: `witchy caps`

Because authority is visible in types, a tool can compute it. `witchy caps`
reports a program's footprint:

```sh
witchy caps program.witchy
```

For a program whose `main` takes `Console` and `Dir[Read]`, it prints which
functions demand which capabilities, and the total. Crucially, this is computed
*from the source* — it re-derives what each function actually requires. It is
not a manifest someone wrote down and might have lied in. That distinction is
the entire foundation of the package-manager story later: a dependency's claimed
footprint and its real footprint are the same thing, because the real one is
what gets checked.

## Effects you'd expect to be free aren't

Notice that reading the clock needs a `Clock`. Why? Because the current time is
*input from outside the program* — it's nondeterminism, and a function that
secretly depends on it isn't pure even though it looks like it returns a number.
witchy makes that dependency show up in the type. The same goes for environment
variables (`Env`) and randomness. If a function's result can change based on
something other than its arguments, it needs a capability to reach that
something, and you can see it in the signature.

That's the model: authority is a value, it can't be forged, and it goes only
where you pass it. The next section is about passing *less* than you have.
