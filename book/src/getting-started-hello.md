# Hello, witchy

Create a file `hello.witchy`:

```witchy
fn main(console: Console):
    print(console, "hello, witchy")
```

Run it:

```sh
witchy hello.witchy
```

```text
hello, witchy
```

Small as it is, every part of this program is doing something specific. Let's
take it apart.

## `fn main(...)`

`main` is the entry point, like in C or Rust. But look at its parameter:
`console: Console`. In most languages `main` takes command-line arguments and
nothing else; the ability to *print* is ambient — `print`, `console.log`,
`std::cout` are globally available.

In witchy, printing is an effect, and effects require authority. `Console` is a
**capability**: an unforgeable value that grants the right to write to standard
output. There is no way to conjure one — you can't write `let c = Console()`.
The only `Console` in the whole program is the one the *host* hands to `main`
when the program starts.

## `print(console, "...")`

Because printing needs a `Console`, `print` takes one as its first argument.
This is the pattern you'll see everywhere: an operation that touches the world
takes the capability that authorizes it. A function that never receives a
`Console` can never print — and you can verify that just by reading its
signature.

Try deleting `console` from `main`'s parameters and running it again: the
compiler refuses, because `main`'s body references a `console` that no longer
exists. Authority has to come from somewhere, and the only "somewhere" is the
parameter list.

## Pure by default

Here's a slightly bigger program. Notice that `double` and `classify` take no
capabilities at all:

```witchy
fn double(n: Int) -> Int:
    n * 2

fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"

fn main(console: Console):
    print(console, classify(double(0)))
    print(console, "${double(21)}")
```

```text
zero
42
```

`double` and `classify` are **provably pure**. They cannot print, cannot read a
file, cannot get the time — there is no parameter that would let them, and they
can't fabricate one. This is not documentation or a naming convention you hope
people follow. It is a property the type checker guarantees. Most of any real
witchy program is functions like these; capabilities flow only to the few places
that genuinely need them.

That single idea — authority is a value, and it only goes where you pass it — is
the whole language in miniature. The rest of this book is about living
comfortably inside it.
