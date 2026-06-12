# Appendix: Recipes

The everyday tasks of real programs, each as a complete, copy-pasteable program.
Notice the shape they share: whatever a program needs to *do* in the world — read
files, reach the network, see the environment — it asks for as a parameter of
`main`. The host grants exactly those, and the program can do exactly that much
and no more. If a recipe doesn't name `Net`, it provably can't reach the network.

## Read a file

A read-only `Dir` is a confined view of one directory subtree. `read` and
`exists` take the directory plus a relative path.

```witchy
fn main(console: Console, root: Dir[Read]):
    if exists(root, "notes.txt"):
        print(console, read(root, "notes.txt"))
    else:
        print(console, "no notes yet")
```

## Write a file

Asking for `Dir[Write]` (rather than a full `Dir`) says in the type that this
program writes but never reads.

```witchy
fn main(console: Console, root: Dir[Write]):
    write(root, "out.txt", "hello from witchy\n")
    print(console, "wrote out.txt")
```

## List a directory

`list` returns the entry names in the subtree; `subdir(root, "sub")` mints a
capability confined to a child folder if you want to descend.

```witchy
fn main(console: Console, root: Dir[Read]):
    for name in list(root):
        print(console, name)
```

## Read an environment variable

`Env` is the capability to read the process environment. `get_env` returns an
`Option(String)` — `None` when the variable is unset — so you handle the missing
case explicitly.

```witchy
fn main(console: Console, env: Env):
    match get_env(env, "HOME"):
        Some(h) -> print(console, "HOME is " + h)
        None -> print(console, "HOME is unset")
```

## Command-line arguments

Arguments arrive as a `List(String)` parameter. Returning an `Int` from `main`
sets the process exit code (`0` is success).

```witchy
fn main(console: Console, args: List(String)) -> Int:
    if list.length(args) == 0:
        print(console, "usage: prog <name>")
        1
    else:
        print(console, "hello, " + list.at(args, 0))
        0
```

## Make an HTTP request

`import http` gives a small client over a `Net` capability. `http.get` returns a
`Response`; `status`, `is_success`, and `body` read it back.

```witchy
import http

fn main(console: Console, net: Net):
    let resp = http.get(net, "localhost", 80, "/")
    print(console, "status " + "${http.status(resp)}")
    if http.is_success(resp):
        print(console, http.body(resp))
```

To narrow the network capability itself — a client that can dial out but never
listen — ask for `Net[Connect, Tcp]` instead of a full `Net`, exactly as the
[narrowing chapter](capabilities-narrowing.md) describes. The client composes
with that narrowing: `http.get` itself demands only `Net[Connect, Tcp]` (and
`server.serve` only `Net[Listen, Tcp]`), so a narrowed handle passes straight
through.

For everything else — string manipulation, lists, dicts, sorting, JSON, time —
see the [standard library reference](appendix-stdlib.md) and the `examples/`
directory in the repository, which carries a runnable program for nearly every
feature in this book.

When you're ready to build something larger, `examples/projects/` has complete
multi-rune applications — a todo app, a ledger, a sales report, a dashboard, and
more — each a small project with its own `witchy.toml`, a library rune and an app
rune wired together by a path dependency. They're the closest thing to a template
for real software: copy the shape, `witchy run`, and start editing.
