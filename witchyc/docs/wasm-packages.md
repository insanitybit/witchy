# Packages

A package is Witchy source kept in its own directory, imported by path or by name
(below). Nothing about it is
trusted: it is checked and proved with the program, and what it can do is decided by what the
program hands it. This page is how to write one, use one, and hold one to what it was reviewed
for. Examples: `tests/fixtures/packages/` (`config/`, `download/`, `httpd/`, and an
application beside each).

## Standard library and packages

The standard library is for broadly used protocols and formats such as
HTTP, cryptography, and JSON. Its I/O APIs take explicit capabilities.

The [SQLite package](../packages/sqlite/README.md) is independently
versioned. Applications opt in through their package list and a database
grant. The transaction host currently runs on Wasmtime.

## Writing one


- A package takes its authority as parameters, never from `main`: a socket and a file
  (`download/`), a read-only directory and a clock (`httpd/`), an allocator (`config/`).
  The narrowest thing that does the job is the right parameter: a `Socket` rather than `Net`,
  a `Dir<Read>` subtree rather than the program's whole directory.
- Work that runs as a task takes over what it needs in a closure (`fn() -> (): fetch(move
  download, ...)`). A type holding owners gives a `discard`, so work given up before it starts
  closes what it holds (`Download.discard`, `Connection.discard`).
- It reports what happened with `report(n)` or returns a result; it prints nothing unless it
  is given a console.
- `pub` marks what the program may call; everything else stays inside the file.

## Using one

By name, from a package list kept beside the program (`httpd-app.packages` for
`httpd-app.wy`), or by path:

```
# httpd-app.packages: name, a version label, and where it is (relative to this file)
httpd 1.0: httpd/httpd.wy
```

```
import "@httpd"          # or: import "httpd/httpd.wy"
...
let shared = allocator.share()
let timer = clock.share()
match run_task(&allocator, &listener, 16384, fn() -> (): answer_within(move connection, move shared, move timer, 50)):
```

One list serves the program: a package that imports another by name finds it in the same
list, as with a lock file, so a program has one version of each. The version is a label for
readers; what was reviewed is pinned by the policy's digests, not by the label
(`tests/fixtures/packages/httpd-app.packages`, `unlisted.refused.wy`).

A package imported by name is known by that name, not by where its copy is:

- **It is qualified by its name.** `@hello-world` is `hello_world`, whatever its file is called
  (`named-app.wy`).
- **A policy names it as `@hello-world`.** Its other files are `@hello-world/PATH`, relative to
  its first file's directory, and a line `@hello-world/:` covers all of them. A build from a
  checkout and a build from a registry copy, such as Grimoire's, are therefore held to the same
  line.
- **Its imports stay in its directory.** It may import standard modules, packages by name, and
  files under its own directory. A path out of the directory, including an absolute path or a
  symlink leading out, is refused (`outside.refused.wy`). A build therefore reads only the
  package's own files, not the host's or another package's.
- **It imports only the packages it requires, when its line says which.** A line may end
  `; requires a b`, or `; requires` for none. The package may then import only those packages
  by name (`declared-app.wy`, `undeclared.refused.wy`). Grimoire writes every line this way, so
  a lock's edges are the build's. A line without the clause, and the program itself, may import
  any package of the list.
- **A file belongs to one owner.** The program may not also import a package's file by path
  (`two-owners.refused.wy`). Otherwise import order would decide which policy line governs it.

`run_task` (from `main`) or `race`, `both` and `TaskSet` (inside a task) run a package's work
(docs/wasm-tasks.md).

## Holding it to what was reviewed

1. `witchyc wasm-caps --by-file app.wy` shows, for each file, what it can use, how many of
   its functions have proved contracts, and which perform unchecked memory operations.
2. `witchyc wasm-caps --draft-policy app.wy > app.policy` writes a policy the program fits
   now, one line per file with its digest. Narrow it: a directory line per package, only the
   capabilities it should have, `; no unchecked` where it should perform none.
3. Keep `app.policy` beside `app.wy`. Every build applies it before proving: a package that
   reaches further, or whose code changed since its line's digest, is not built
   (`overreach.refused.wy`). Paths are relative to the policy's directory.
4. After reviewing an update to a package, `witchyc wasm-caps --repin app.policy app.wy`
   sets its digest to the new code (and changes nothing else in the file).

```
<standard>/: *
httpd-app.wy: *
@httpd/: Allocator, Clock, Dir<Read>, Socket ; no unchecked ; digest 559f5797...
```

A file no line covers is refused whatever it does, so a new dependency is reviewed before it
is allowed anything. Details: docs/wasm-capabilities.md.

## Not yet

- The compiler fetches nothing. A list names what is already on disk. Grimoire fetches
  verified copies from a local Coven registry and writes the list (Grimoire's
  `examples/registry`).
