# Hello, witchy

Create a file `hello.witchy`:

```witchy
fn main(console: Console):
    console.print("hello, witchy")
```

Run it:

```sh
witchy hello.witchy
```

```text
hello, witchy
```

The entry point and its `Console` parameter establish the capability model.

## `fn main(...)`

`main` is the entry point, like in C or Rust. Its parameter is
`console: Console`. Printing is an effect, so the program receives a `Console`
value for it.

In witchy, printing is an effect, and effects require authority. `Console` is a
**capability**: a value that grants the right to write to standard output - and
one your code can't forge. There's no way to conjure one - you can't write
`let c = Console()`.
The only `Console` in the whole program is the one the *host* hands to `main`
when the program starts.

## `console.print("...")`

Because printing needs a `Console`, `print` is a method on one. Operations that
touch the world are reached through the capability that authorizes them. A
function that receives no `Console` has no printing operation in its signature.

Try deleting `console` from `main`'s parameters and running it again: the
compiler rejects it, because `main`'s body references a `console` that no longer
exists. Authority has to come from somewhere, and the only "somewhere" is the
parameter list.

## Pure by default

In this larger program, `double` and `classify` take no capabilities:

```witchy
fn double(n: Int) -> Int:
    n * 2

fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"

fn main(console: Console):
    console.print(classify(double(0)))
    console.print("${double(21)}")
```

```text
zero
42
```

`double` and `classify` are **provably pure**. They can't print, can't read a
file, can't get the time - there's no parameter that would let them, and they
can't fabricate one. This isn't documentation or a naming convention you hope
people follow. It's a property the type checker guarantees. Most of any real
witchy program is functions like these; capabilities flow only to the few places
that genuinely need them.
