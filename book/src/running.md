# Running Witchy Code

Once a program type-checks, there are several ways to run it. They all use the
same capability contract, but they differ in who supplies authority and how
strictly the host is confined around the guest.

Witchy's production execution path compiles to WebAssembly. The tree-walking
interpreter is an independent semantic oracle used by `witchy parity` and the
test suite; a normal run does not pay for executing both backends.

## Run a source file

The shortest development loop points the CLI directly at a source file:

```sh
witchy hello.witchy
```

The host constructs the parameters of `main`. Authority is explicit in the
signature:

```witchy
fn main(console: Console):
    console.print("ready to run")
```

```text
ready to run
```

A function that receives no `Dir`, `File`, `Fetch`, `Net`, or `Exec` cannot
perform those effects. Direct development runs provide convenient development
grants; use the sandbox when the grant itself must be explicit and reviewable.

## Build and run a rune

A multi-file project is a *rune*. These commands are deliberately separate:

```sh
witchy build .
witchy run . arg1 arg2
```

`build` resolves and verifies dependencies, runs accepted confined build steps,
links, and type-checks. It does not execute the application. `run` performs that
build pipeline and then invokes the rune's application entrypoint.

Commit `witchy.lock`. It records the resolved source identities, hashes,
provenance, and per-rune runtime/build capability footprints. See
[The Manifest, the Lockfile, and the CLI](packages-cli.md) for the project
workflow.

## Run with an explicit grant

`witchy sandbox` computes the program's footprint, compiles it, and links only
the host operations that footprint may use:

```sh
witchy sandbox --dir ./data report.witchy
witchy sandbox --fetch https://api.example.com client.witchy
witchy sandbox --grants app.grants.toml --accept-grants app.witchy
```

Directory and file grants are handle-anchored. Network and Fetch grants carry
allowlists. Named entries in a grant document bind to same-named parameters of
`main`, and the document is checked against the footprint before execution.
The full contract is in [The Sandbox](capabilities-sandbox.md).

For deployments that must refuse execution unless every implemented outer
native layer is active, add `--confine=required`:

```sh
witchy sandbox --confine=required --dir ./data report.witchy
```

The default is best effort because kernel enforcement differs by host. This
outer fence is defense in depth; the capability boundary remains enforced by
the linked host interface either way.

## Check both semantic implementations

Use parity when changing language or standard-library behavior:

```sh
witchy parity hello.witchy
```

It runs the same checked program through the interpreter and compiled
WebAssembly backend and fails if output or error behavior differs. The
repository's differential suites apply the same oracle to language, standard
library, capability, and book examples.

## Run in the browser

The compiler itself builds to WebAssembly. In this book, each supported example
is compiled by the trusted page and then executed in a fresh sandboxed iframe
with an opaque origin. The child frame receives only the compiled guest and
plain grant data; its Content Security Policy admits exactly its granted
`Fetch` origins.

The browser has honest providers for pure computation, `Console`, `Clock`,
page-supplied `Env`, an in-memory `Dir`, origin-scoped `Fetch`, opaque
`SecretStore`, and sequential `VM`. Raw sockets, native subprocesses, bare
secrets, and host filesystem roots have no browser provider and remain denied.
There is no fallback to execution in the main page.

## Choose the boundary

| Form | Authority comes from | Appropriate use |
|---|---|---|
| Direct source or `witchy run` | developer host defaults and flags | local development |
| `witchy sandbox` | runner-supplied flags or grant document | explicit, reviewable untrusted-guest execution |
| Portable `.wasm` in the sandbox | artifact consumer | distribution without trusting the author beyond the sandbox |
| Trusted executable | authenticated author binding plan | installing an application whose author, runtime, and distributor are trusted |
| Browser host | embedding page's published capability menu | interactive examples and web applications |
