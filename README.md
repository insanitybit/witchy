# witchy

A capability-secure language where a program's authority is a **typed, auditable,
diffable, enforceable artifact**.

```witchy
fn load(dir: Dir[Read], name: String) -> String:    // provably cannot write
    read(dir, name)

fn main(console: Console, dir: Dir):
    print(console, load(dir, "notes.txt"))           // full Dir narrows to Dir[Read]
```

# Why witchy?

Supply chain security is a serious problem. Packages have very good reasons for running
code during install time, yet the capability is all or nothing. Libraries in most
languages have no means by which they can be restricted. It's very hard to know that
`some_library::foo()` doesn't delete your file system or steal your secrets.

`witchy` aims to take supply chain security very seriously across its entire stack of
language features and tooling. The language itself is capability based, allowing you to
write code like this:
```
import http        // stdlib HTTP client over `Net` — trusted, you hand it `net`
import markdown    // third-party rune; vetted as pure (footprint: [])

fn main(console: Console, net: Net, dir: Dir):
    // You genuinely hold both caps and use them legitimately.
    let template = read(dir, "report.tmpl")
    let notes    = http.body(http.get(net, "notes.internal", 443, "/today"))

    // Hand the untrusted rune the *data*, never the *authority*. Inside the
    // seal `net` and `dir` don't exist, so nothing here — markdown or anything
    // it calls — can be handed them. Only the rendered String crosses back out.
    let html = without net, dir:
        markdown.render(template, notes)

    print(console, html)
```

This is extremely hard to mess up. `without` drops the capabilities from the scope entirely,
even if `render` changed to require a `net` you *couldn't* pass it in without addressing the
explicit `without` block too.

This extends out to build time execution. Build scripts are full `witchy` programs, but once
again we can reason about those in terms of their capabiltiies. A dependency `foo` that only
requires `Clock` gets compromised, now it asks for `Net` - that gets flagged!
```
$ witchy update
blocked: acme/logger 1.0.0 -> 1.1.0 widens the footprint
  + Net
run `witchy update --allow-cap Net` to accept, or pin the old version
```
No hidden "suddenly my dependency is pulling from ghostbin".

## A disclosure

`witchy` is a project for fun. It's vibe coded using different models. It's a way
for me to explore ideas in this space. If that's not for you, no problem! But you
should be aware, upfront, that this project oscillates heavily between "these changes
were meaningfully reviewed" and "I literally didn't even check". I have also not
invested much into the AI side of things! In an ideal world I would ensure there
are skills, tools, etc, to assist the AI in doing things properly but that's just
not the case at all today.

Eventually, I'd love to rewrite docs myself by hand. For now they are mostly AI generated.

## New here? Run the docs

The guided way to learn witchy is **[The witchy Book](book/src/SUMMARY.md)**. Run
it locally with live reload:

```sh
git clone https://github.com/insanitybit/witchy && cd witchy
cargo install mdbook              # one-time, if you don't already have it
./scripts/build-book.sh --serve   # builds the book and opens it in your browser
```

That's a chapter-by-chapter tour from "hello" through capabilities, actors,
generators, and the package manager — every example is run and verified by the
test suite, so what you read is exactly what the language does. (No witchy build
needed just to read it.)

Want to *run code* with zero install instead? The [playground](#playground) runs
the real interpreter in your browser. Or jump straight to [Install](#install) to
build the `witchy` CLI.

Authority enters a witchy program in exactly one place: the typed parameters of
`main`, minted by the host. A `Console`, a `Dir[Read]`, a `Net[Connect, Tcp]` —
capabilities are unforgeable values that propagate only as function arguments,
are visible in every signature, and can only ever be *narrowed*, never widened.
Calling an effectful operation without the capability is a **compile-time
error**. A function with no capability parameters provably has no effects.

That makes three things possible that mainstream languages can't offer:

1. **Audit by reading signatures.** `witchy caps program.witchy` recomputes the
   program's full capability footprint from source — per function, per right
   (`Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs `Net[Listen]`). It is never
   self-asserted metadata.
2. **Gate on widening.** `witchy caps-diff old new` exits non-zero when authority
   grew — wire it into CI and a dependency can't quietly gain `Net[Listen]`.
   The package manager (see below) blocks `add`/`update` on widening.
3. **Enforce at runtime.** `witchy sandbox program.witchy` compiles to
   WebAssembly and runs it in a VM granted exactly the computed footprint —
   the module physically has no other host imports to call.

## Three backends, one semantics

| Backend | Command | Use |
|---|---|---|
| Tree-walking interpreter | `witchy run.witchy` | Development; the reference semantics |
| WebAssembly (wasmtime) | `witchy sandbox run.witchy` | Confinement: the capability boundary is the VM boundary |
| Native (Rust transpilation) | `witchy native run.witchy` | Speed |

The backends are held to a **zero-silent-divergence** invariant: `witchy parity
<file>` runs a program on both the interpreter and the compiled WASM and
confirms identical output — including agreement on *error paths* (an
out-of-bounds index traps on both, an unparseable integer fails on both). The
test suite contains hundreds of differential tests and a property-based fuzzer
holding the backends together.

## The language in 30 seconds

Indentation-based layout, expression-oriented, statically typed with inference:

```witchy
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:                       // exhaustiveness-checked
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    let shapes = [Circle(2), Square(3)]
    for s in shapes:
        print(console, "area: ${area(s)}")    // string interpolation
```

- `Int` (64-bit), `Float`, `Bool`, `String`, `Duration` (native literals: `30s`,
  `2hr`), `List(a)`, `Dict(k, v)`, tuples, records, ADTs, `Option`/`Result`
  with the `?` operator.
- Traits with `where` bounds, monomorphized for compiled backends.
- Hylo-style parameter conventions: `let` (immutable borrow, the default),
  `inout` (caller's variable is written back), `sink`/`own` (ownership
  transfer; use-after-move is a compile error).
- Actors with typed handlers, isolated per-VM when compiled.
- Structural equality, deep, on both backends.

## Package manager: `coven`

Packages ("runes") publish to a content-addressed, signed registry. The
distinguishing rule: **the registry and the client recompute every rune's
capability footprint from source — declared metadata is never trusted**, and
`witchy add`/`update` **block when a dependency's footprint widens** until you
explicitly approve. Two-phase publish (stage → 2FA promote), TUF-style metadata
against rollback/freeze, keyless OIDC trusted publishing, lockfiles, vendoring.
Dependency code never executes at build time.

The package manager and registry are **self-hosted**: `projects/pm` and
`projects/coven` are witchy programs, exercised end-to-end by the test suite.

Try the whole lifecycle locally — server, trusted publish, 2FA promote,
verified consumption — with one command:

```sh
./scripts/local-registry-demo.sh
```

See [docs/local-registry.md](docs/local-registry.md) for the step-by-step
version, and [docs/package-manager.md](docs/package-manager.md) for the full
design and threat model.

## Install

From source (requires a Rust toolchain):

```sh
git clone https://github.com/insanitybit/witchy
cd witchy
cargo build --release
./target/release/witchy examples/hello.witchy
```

## CLI

```
witchy [--net <host:port>]... <file.witchy>   run a program
witchy check    <file.witchy>                 type-check without running
witchy parity   <file.witchy>                 run on both backends, confirm identical output
witchy sandbox  <file.witchy>                 compile and run in a VM granted exactly its footprint
witchy native [-o out] <file.witchy>          compile to a native binary via rustc/LLVM
witchy emit-wat <file.witchy>                 print the compiled WebAssembly text
witchy emit-rust <file.witchy>                print the native (Rust) transpilation
witchy caps     <file.witchy>                 report the capability footprint (runtime + build axes)
witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened on either axis
witchy build-step <file.witchy>               run a rune's build step under confined grants
                                              ([--out <dir>] [--read <dir>] [--env K]... [--exec tool]...)
witchy test     <file.witchy|dir>             run in-language tests
witchy fmt [--check] <file.witchy>            reformat (--check: verify only)
witchy doc      <file.witchy>                 extract Markdown API docs
witchy lsp                                    run the language server
witchy new/add/build/run/publish/...          package-manager commands
```

## Playground

The witchy interpreter compiles to WebAssembly, so it runs **in the browser**
with no server. Build the page and open it:

```sh
./scripts/build-playground.sh
python3 -m http.server -d web 8000   # then visit http://localhost:8000
```

`web/` is a static page (`index.html` + `playground.js`) that loads the
interpreter module and runs snippets client-side; it ships with the language
reference's examples preloaded.

## Learn more

- **[The witchy Book](book/src/SUMMARY.md)** — the guided, chapter-by-chapter
  introduction (build it with `./scripts/build-book.sh`, or read the chapters as
  Markdown under `book/src/`). Start here if you're new.
- **[Language reference](docs/language.md)** — the full syntax and semantics.
- **[Capabilities guide](docs/capabilities.md)** — the security model, for users.
- **[Standard library](docs/stdlib.md)** — 30 modules, function-by-function.
- **[Examples](examples/)** — 100+ programs from `hello` to a self-hosted
  package registry; see the [index](examples/README.md).
- **[Capability rights design](docs/capability-rights.md)** and
  **[package-manager design](docs/package-manager.md)** — the deeper design docs.
- **[Architecture](docs/architecture.md)** — how the compiler and backends fit
  together, and the parity discipline that keeps them honest.

Editor support: a [Zed extension](editors/zed) with tree-sitter highlighting and
`witchy lsp` diagnostics.

## Status

Witchy is a young language (pre-1.0). The capability model, the three backends
and their parity discipline, the formatter, the LSP, and the package-manager
core are implemented and tested (1,100+ tests). The build-time capability system
is partially built: the build footprint is computed and gated (`witchy caps` /
`caps-diff`, both axes) and build steps run confined on the interpreter
(`witchy build-step`); still pending are the zero-ambient WASM-sandbox execution
path, `BuildNet`, and the per-rune `[build.grants]` manifest/lockfile/registry
integration (see [docs/build-time-execution-plan.md](docs/build-time-execution-plan.md)).
Also not yet done: a hosted public registry and performance work. See
[docs/architecture.md](docs/architecture.md) for the honest limitations list,
including the WASM memory model.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
