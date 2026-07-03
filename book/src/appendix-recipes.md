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

Where does `root` point? A plain `witchy program.witchy` run roots every `Dir`
it grants at the **current working directory** — so relative paths resolve
against where you launched the program, not where the source file lives. To run
it confined to a specific subtree instead, use `witchy sandbox --dir <root>
program.witchy`; the sandbox prints exactly what it granted. Either way the
program only ever sees paths *under* that root — `subtree(root, "sub")` narrows
further to a child folder.

## Read just one file

When a program needs *one* file, ask for a `File[Read]` instead of a whole `Dir` —
the least authority that does the job. A `Dir` navigates down to a single file with
`read_file`, and a `File` op takes no path (it *is* the file):

```witchy
// `load` provably touches one file — `witchy caps` reports it as `File[Read]`,
// never `Dir`, so it cannot see anything else in the tree.
fn load(cfg: File[Read]) -> String:
    read(cfg)

fn main(console: Console, root: Dir[Read]):
    print(console, load(root.read_file("config.toml")))
```

`root.read_file(...)` needs `Dir[Read]` and yields `File[Read]`; the write
counterpart is `root.write_file(...)` (needs `Dir[Write]`, yields `File[Write]`).
A single-file program can skip the `Dir` entirely and take the file straight from
the host: `fn main(console: Console, cfg: File[Read])`, granted with
`witchy sandbox --file config.toml program.witchy`.

## Write a file

Asking for `Dir[Write]` (rather than a full `Dir`) says in the type that this
program writes but never reads.

```witchy
fn main(console: Console, root: Dir[Write]):
    write(root, "out.txt", "hello from witchy\n")
    print(console, "wrote out.txt")
```

## List a directory

`list` returns the entry names in the subtree; `subtree(root, "sub")` mints a
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
    if args.length() == 0:
        print(console, "usage: prog <name>")
        1
    else:
        print(console, "hello, " + args.at(0))
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

## Fetch an untrusted URL without DNS rebinding

When the URL comes from outside — a webhook target, a user-supplied link — a
plain fetch is exposed to a rebinding attack: the name is resolved once for a
safety check and *again* at connect time, and an attacker who controls the DNS
can return an internal address the check never saw. `http.pin` closes that gap.
It resolves the host **once**, lets your policy approve one of the resolved IPs,
and returns a sealed `PinnedUrl`; `http.get_pinned` dials **exactly that IP** —
never re-resolving — while still presenting the original hostname for TLS and the
`Host` header. Pair it with a `Net` that denies the private ranges, so the
capability itself is the hard floor even if the policy is wrong:

```witchy
import http
import confine

fn main(console: Console, net: Net[Connect, Tcp]):
    let safe = net.deny(confine.private())
    match http.pin(safe, "http://example.com/status", public_ok):
        Err(e) -> print(console, "blocked: " + e)
        Ok(target) ->
            match http.get_pinned(safe, target):
                Ok(resp) -> print(console, "status " + "${http.status(resp)}")
                Err(e) -> print(console, "fetch failed: " + e)

// Your policy: inspect a resolved IP literal and approve or reject it. Returning
// `false` for every candidate makes `pin` fail closed.
fn public_ok(ip: String) -> Bool:
    true
```

Because `PinnedUrl` is a sealed capability — only `std/http` can mint one — a
value of that type is unforgeable proof that the policy ran: a function that asks
for a `PinnedUrl` cannot be handed an unchecked target.

## Render HTML with Glamour

[Glamour](https://github.com/insanitybit/witchy) is witchy's frontend framework
(this very book is a Glamour app). A view is built as **data** — a tree of `VNode`s,
never a string — and rendered to HTML. Because `text` is escaped by construction,
there is no HTML-injection sink: a `<script>` in your data renders as inert text, not
markup. Try running this — it needs no capability but `Console`, so it executes right
here in the page:

```witchy
import glamour

fn main(console: Console):
    let view = glamour.element("article", [glamour.prop("class", "post")], [
        glamour.element("h1", [], [glamour.text("Hello from a rune")]),
        glamour.element("p", [], [glamour.text("Glamour renders <script> as text.")]),
    ])
    print(console, glamour.to_html(view))
```

Beyond rendering, a full Glamour app adds an MVU loop (`view`/`update`/`step_with`) and
effects-as-data (`Cmd`s the host performs), with UI authority — fetch, routing, timers —
carried as capabilities (`UiFetch`, `UiRoute`, …) narrowed from a single app-root
`UiRoot`, exactly like every other capability in witchy.

Here is a **live** one — a real Glamour counter (`view`/`update`, clickable buttons)
compiled to WebAssembly and mounted by the very same runtime that renders this book. Its
network authority (`UiFetch`) is **denied**, so it can compute and render but can't phone
home — the capability model, running in the page:

```glamour-app
counter
```

For everything else — string manipulation, lists, dicts, sorting, JSON, time —
see the [standard library reference](appendix-stdlib.md) and the `examples/`
directory in the repository, which carries a runnable program for nearly every
feature in this book.

When you're ready to build something larger, `examples/projects/` has complete
multi-rune applications — a todo app, a ledger, a sales report, a dashboard, and
more — each a small project with its own `witchy.toml`, a library rune and an app
rune wired together by a path dependency. They're the closest thing to a template
for real software: copy the shape, `witchy run`, and start editing.
