# Project: A Confined Log Scanner

This chapter builds `scan`, a confined command-line tool that searches a log file
for lines containing a query, with optional case-insensitivity. It exercises a
pure core, a capability shell, and the sandbox. The organizing rule is: *push
the effects to the edges, keep the middle pure.*

## Start pure

Filtering lines needs no capabilities. Write that logic as plain functions so it
can be run and tested independently:

```witchy
// Lines of `contents` that contain `query`.
fn matches(query: String, contents: String) -> List(String):
    var hits = []
    for line in contents.lines():
        if line.contains(query):
            hits.push(line)
    hits

// Case-insensitive variant: fold both sides to lower case first.
fn matches_ci(query: String, contents: String) -> List(String):
    var hits = []
    let needle = query.to_lower()
    for line in contents.lines():
        if line.to_lower().contains(needle):
            hits.push(line)
    hits

fn main(console: Console):
    let log = "INFO started\nWARN disk low\ninfo retry\nERROR boom"
    for line in matches("INFO", log):
        console.print("exact:  ${line}")
    for line in matches_ci("info", log):
        console.print("ci:     ${line}")
```

```text
exact:  INFO started
ci:     INFO started
ci:     info retry
```

This is the heart of the program, and it's *provably effect-free* - look at the
signatures. We could write a dozen `test_*` functions for it and
never need a capability. That's the goal: the logic that's worth testing
carefully is the logic that touches nothing.

## Add the capability shell

Now the thin outer layer that reads the file and decides which variant to use.
This part needs authority, and its signature says exactly which:

```witchy
fn matches(query: String, contents: String) -> List(String):
    var hits = []
    for line in contents.lines():
        if line.contains(query):
            hits.push(line)
    hits

fn matches_ci(query: String, contents: String) -> List(String):
    var hits = []
    let needle = query.to_lower()
    for line in contents.lines():
        if line.to_lower().contains(needle):
            hits.push(line)
    hits

// The entry point: Console to print, Dir[Read] to read the log, Env to check a
// setting, and the command-line arguments. Returns an Int exit code.
fn main(console: Console, dir: Dir[Read], env: Env, args: List(String)) -> Int:
    let query = args.at(0)
    let path = args.at(1)
    let contents = dir.read(path)
    let insensitive = match env.get_env("SCAN_IGNORE_CASE"):
        Some(_) -> true
        None -> false

    let hits = if insensitive: matches_ci(query, contents) else: matches(query, contents)
    for line in hits:
        console.print(line)
    0
```

Read that `main` signature and you know the program's *entire* capability
footprint: it prints, reads one directory subtree (read-only), reads environment
variables, and takes arguments. The type contains no file-writing or network
capability. The next step proves the footprint.

## Audit it

```sh
witchy caps scan.witchy
```

```text
main   Console, Dir[Read], Env
total  Console, Dir[Read], Env
```

`Dir[Read]`, not `Dir` - the analyzer sees that we only ever call `read`, never
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
would fail because the required host function is absent.

Try the confinement yourself: a path like `../../etc/passwd` is rejected by the
`Dir`'s confinement, because `scan` was rooted at `./logs` and `..` can't escape
it.

## Program shape

We split the program into a pure core and a capability shell, audited its exact
authority from the source, and ran it in a VM that can do precisely that and
nothing more. That shape - pure middle, thin authorized edge, enforced boundary -
is how witchy programs are meant to be built. The bigger the program, the more
it pays off: the surface that can affect the world stays small and legible, and
everything else is provably inert.
