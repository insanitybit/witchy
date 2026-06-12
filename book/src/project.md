# Project: A Confined Log Scanner

Time to build something real and run it confined. We'll write `scan`: a
command-line tool that searches a log file for lines containing a query, with
optional case-insensitivity. It's small, but it exercises the whole stack — a
pure core, a capability shell, and the sandbox — and it shows the witchy way of
structuring a program: *push the effects to the edges, keep the middle pure.*

## Start pure

The actual work — filtering lines — needs no capabilities at all. So we write it
as plain functions, and we can run and test them immediately:

```witchy
import string

// Lines of `contents` that contain `query`.
fn matches(query: String, contents: String) -> List(String):
    var hits = []
    for line in string.lines(contents):
        if string.contains(line, query):
            hits = list.push(hits, line)
    hits

// Case-insensitive variant: fold both sides to lower case first.
fn matches_ci(query: String, contents: String) -> List(String):
    var hits = []
    let needle = string.to_lower(query)
    for line in string.lines(contents):
        if string.contains(string.to_lower(line), needle):
            hits = list.push(hits, line)
    hits

fn main(console: Console):
    let log = "INFO started\nWARN disk low\ninfo retry\nERROR boom"
    for line in matches("INFO", log):
        print(console, "exact:  " <> line)
    for line in matches_ci("info", log):
        print(console, "ci:     " <> line)
```

```text
exact:  INFO started
ci:     INFO started
ci:     info retry
```

This is the heart of the program, and it's *provably effect-free* — look at the
signatures. We could write a dozen `test_*` functions for it (next chapter) and
never need a capability. That's the goal: the logic that's worth testing
carefully is the logic that touches nothing.

## Add the capability shell

Now the thin outer layer that reads the file and decides which variant to use.
This part needs authority, and its signature says exactly which:

```witchy
import option
import string

fn matches(query: String, contents: String) -> List(String):
    var hits = []
    for line in string.lines(contents):
        if string.contains(line, query):
            hits = list.push(hits, line)
    hits

fn matches_ci(query: String, contents: String) -> List(String):
    var hits = []
    let needle = string.to_lower(query)
    for line in string.lines(contents):
        if string.contains(string.to_lower(line), needle):
            hits = list.push(hits, line)
    hits

// The entry point: Console to print, Dir[Read] to read the log, Env to check a
// setting, and the command-line arguments. Returns an Int exit code.
fn main(console: Console, dir: Dir[Read], env: Env, args: List(String)) -> Int:
    let query = list.at(args, 0)
    let path = list.at(args, 1)
    let contents = read(dir, path)
    let insensitive = match get_env(env, "SCAN_IGNORE_CASE"):
        Some(_) -> true
        None -> false
    let hits = if insensitive: matches_ci(query, contents) else: matches(query, contents)
    for line in hits:
        print(console, line)
    0
```

Read that `main` signature and you know the program's *entire* capability
footprint: it prints, it reads one directory subtree (read-only — it can't
modify your logs), it reads environment variables, and it takes arguments. It
cannot write files. It cannot open a network connection. Not "it doesn't"; it
*can't*, and the next step proves it.

## Audit it

```sh
witchy caps scan.witchy
```

```text
main   Console, Dir[Read], Env
total  Console, Dir[Read], Env
```

`Dir[Read]`, not `Dir` — the analyzer sees that we only ever call `read`, never
`write`. If we later added a `write` call, this output would change, and a
`caps-diff` in CI would flag the new authority.

## Run it confined

```sh
witchy sandbox --dir ./logs scan.witchy ERROR app.log
```

The sandbox grants `scan` exactly its footprint: a `Console`, a read-only `Dir`
rooted at `./logs`, `Env`, and the arguments `ERROR app.log`. Inside the VM there
is no `write` import, no `Net` import, no `Dir` outside `./logs`. If a typo (or a
malicious edit) made `scan` try to write a file or phone home, instantiation
would fail — the host function it needs simply wouldn't be there.

Try the confinement yourself: a path like `../../etc/passwd` is rejected by the
`Dir`'s confinement, because `scan` was rooted at `./logs` and `..` can't escape
it.

## What we did

We split the program into a pure core and a capability shell, audited its exact
authority from the source, and ran it in a VM that can do precisely that and
nothing more. That shape — pure middle, thin authorized edge, enforced boundary
— is how witchy programs are meant to be built. The bigger the program, the more
it pays off: the surface that can affect the world stays small and legible, and
everything else is provably inert.

Next, how to share code like this without giving up any of those guarantees.
