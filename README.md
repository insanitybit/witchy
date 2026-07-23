# witchy

Witchy is an experimental capability-secure language. Host authority is explicit
in program types and artifacts, so it can be inspected, narrowed, diffed, and
checked at defined compiler/runtime boundaries.

The dependable public-preview path is deliberately smaller than the repository:
ordinary language fundamentals, capability inspection, checking/formatting/testing,
interpreter-versus-WASM parity, portable WASM sandboxing, and deliberately trusted
self-contained executables. Package distribution, Coven, Glamour, advanced
language mechanisms, and editor tooling remain available as experimental work.
See [PRODUCT-STATUS.md](PRODUCT-STATUS.md) for the evidence-backed boundary.

```witchy
// This helper receives read authority, not write authority.
fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir):
    // Full Dir narrows to Dir[Read].
    console.print(load(dir, "notes.txt"))
```

## Quick start

From an authorized source checkout, build with a Rust toolchain and exercise the
small supported path:

```sh
cargo build --release
witchy=./target/release/witchy
$witchy check examples/hello/src/hello.witchy
$witchy examples/hello/src/hello.witchy
$witchy parity examples/hello/src/hello.witchy
$witchy test examples/hello
```

The [capability chapters](book/src/capabilities.md) continue from there with
explicit `Dir`, `File`, `Net`, and secret grants.

## Why capabilities?

A Witchy function cannot exercise host authority that is absent from its typed
inputs. In the opening example, `load` accepts `Dir[Read]`, so its interface
cannot request a write operation. Giving a helper plain data instead of `Dir` or
`Net` also prevents that helper from directly exercising those host capabilities.

This is a bounded guarantee, not a claim that arbitrary Witchy software is
automatically safe. The compiler, runtime, selected host bindings, and distributor
remain trusted. `witchy caps` reports demanded authority, `caps-diff` detects a
widening, and `sandbox` rejects missing grants at the host boundary. The tests are
concrete regression evidence; they are not an independent security audit.

## What is supported today

The supported-preview contract covers:

- source checking, formatting, execution, and in-language tests;
- core static language features used by the guided capability journey;
- capability footprints, rights narrowing, grant checking, and sandbox failures;
- portable WASM compilation and sandboxed execution;
- fail-closed interpreter/compiled-WASM parity checking;
- small local single-rune project workflows; and
- `trusted-exe`, with its whole-artifact trust model and checked root bindings.

Everything else is classified explicitly in [PRODUCT-STATUS.md](PRODUCT-STATUS.md).
Experimental does not mean abandoned or forbidden: it means the feature may
evolve without being part of this dependable path.

## Language stability

Witchy is pre-1.0 and compatibility-unstable. Language, standard-library,
package, artifact, and CLI behavior may make breaking changes without a
deprecation period. A feature merging does not automatically promote it into the
supported-preview contract.

The parity, checked-heap, sandbox, and artifact tests provide concrete regression
evidence; they are not proof that the compiler or runtime has no defects. Native
filesystem capabilities are handle-anchored across every path component: reads,
writes, navigation, listing, creation, direct `File` grants, and build roots
cannot be redirected by replacing a parent. Executable selection uses descriptor
execution, an already-open private snapshot, or, on macOS, a path whose opened
identity and root-owned non-writable ancestry were verified before execution;
mutable grant paths are never reopened for execution. Strict Linux launches add
an independently derived Landlock/seccomp fence, and runnable book cells execute
inside fresh opaque-origin frames whose CSP admits only their granted Fetch
origins. Required mode fails before `main` when every requested outer layer
cannot be enforced; other hosts report best-effort degradation explicitly.

Portable `.wasm` artifacts remain untrusted guests run by a separately installed
Witchy host. A [`trusted-exe`](rfcs/0092-trusted-application-executables.md), by contrast, embeds the application, checked root
capability bindings, and the native Witchy runtime in one executable. Running it
trusts that complete artifact and its distributor; capabilities constrain
delegation inside the trusted application, not the trusted root's authority over
the user. Embedded digests detect corruption but do not authenticate a publisher.
Grimoire/Coven integrated installation, runtime `Dynamic`, and unaccepted lexical
extensions are proposals rather than supported product behavior.

## AI disclosure

Witchy is developed extensively with AI assistance. Models contribute compiler,
runtime, test, documentation, and RFC work. Human judgment owns product goals,
language and capability-model decisions, priorities, and public claims.

AI-generated code or prose does not imply correctness or independent security
review. Supported behavior is determined by executable evidence, stated trust
boundaries, known limitations, and the tracked maturity contract—not by who or
what produced the implementation. Contributions should disclose material AI use,
including the model used.

## Read the docs

The guided path is **[The Witchy Book](book/src/SUMMARY.md)**. Read the Markdown
chapters directly under `book/src/`. The repository also dogfoods the experimental
[Glamour](projects/glamour/README.md) frontend by rendering the book as a
client-side app with editable cells. Build that private-development bundle with:

```sh
cargo build --release            # the toolchain (compiler + native host)
./scripts/build-docs.sh dist     # assemble the bundle + a current browser compiler
python3 -m http.server -d dist 8000   # then open http://localhost:8000
```

Checked and runnable code blocks are exercised by documentation and backend-parity
tests. Prose and experimental chapters remain subject to the maturity boundary in
[PRODUCT-STATUS.md](PRODUCT-STATUS.md).

## The language in 30 seconds

Indentation-based layout, expression-oriented, statically typed with inference:

```witchy
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    // Exhaustiveness-checked.
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    let shapes = [Circle(2), Square(3)]
    for s in shapes:
        // String interpolation.
        console.print("area: ${area(s)}")
```

- `Int` (64-bit), `Float`, `Bool`, `String`, `Duration` (native literals: `30s`,
  `2hr`), `List(a)`, `Dict(k, v)`, tuples, records, ADTs, `Option`/`Result`
  with the `?` operator.
- Traits with `where` bounds, monomorphized for the compiled backend.
- Hylo-style parameter conventions: `let` (immutable borrow, the default),
  `var` (caller's variable is written back), `own` (ownership transfer;
  use-after-move is a compile error).
- Structural equality, deep, on both backends.

Async/channels, generators, reflection/comptime, regions, and optimization modes
are implemented experimental surfaces rather than prerequisites for the core
capability journey.

## Experimental: packages and Coven

Witchy's self-hosted package-manager client ([`projects/pm`](projects/pm)) and
registry ([`projects/coven`](projects/coven)) explore source-recomputed
capability footprints, widening gates, signed records, vendoring, build-step
confinement, and trusted publishing. They have substantial tests but remain an
evolving end-to-end identity, update, build, and operations contract.

Use them as development dogfood, not as evidence that Witchy currently provides
a supported hosted registry or independently audited supply-chain system. The
design and threat model live in [rfcs/package-manager.md](rfcs/package-manager.md).

## CLI

The supported-preview commands are:

```text
witchy [--net <host:port>]... [--fetch <origin>]... <file.witchy>
                                              run a program
witchy check    <file.witchy>                 check + verify compiled acceptance without running
witchy parity   <file.witchy>                 run on both backends, confirm identical output
witchy test     <file.witchy|dir>             run in-language tests
witchy fmt [--check] <file.witchy>...         format or verify canonical formatting
witchy caps     <file.witchy>                 report the host-capability footprint
witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widens
witchy grants-check <program> <grants.toml>   verify demanded authority fits a grant document
witchy sandbox [--dir <root>] [--net <addr>]... [--fetch <origin>]... <file.witchy> [args...]
                                              compile and run in a VM granted exactly its footprint
witchy compile <file.witchy> --out <file.wasm>
                                              emit portable WebAssembly for the sandbox host
witchy emit-wat <file.witchy>                 print the compiled WebAssembly text
witchy --release build --target trusted-exe   build one trusted, self-contained native application
                                              (running it trusts app + embedded runtime + distributor)
```

Authoring helpers, multi-package/registry commands, build steps, Coven, LSP, and
optimization counters are classified separately in
[PRODUCT-STATUS.md](PRODUCT-STATUS.md). Run `witchy --help` for the exact current
syntax; this list is intentionally the curated product path rather than an
exhaustive inventory.

## Experimental: browser playground

Witchy's compiler — front end, type checker, and the WIR → wasm backend —
itself compiles to WebAssembly, so the playground runs **in the browser** with
no server: it compiles your snippet to a wasm binary and instantiates *that* on
the browser's own WebAssembly engine. It is the same compiler and backend as
native `witchy`, run under a different host: the browser uses the pure-compute,
capability-denied host (infrastructure imports like `print` are provided;
`Dir`/`Net`/`Clock`/`Env`/`Exec`/secrets are omitted), so a pure snippet runs
while one that reaches for host authority fails to instantiate. Native `witchy
sandbox`, by contrast, links the wasmtime host with the program's granted
footprint. Build the page and open it:

```sh
./scripts/build-playground.sh
python3 -m http.server -d web 8000   # then visit http://localhost:8000
```

`web/` is a static page (`index.html` + `playground.js`) that loads the
compiler module, compiles snippets to wasm client-side, and runs them; it ships
with language examples preloaded. This is active dogfood, not yet a supported
hosted service or general browser-application platform.

## Learn more

- **[The witchy Book](book/src/SUMMARY.md)** — the guided, chapter-by-chapter
  introduction (a client-side Glamour app: build it with `./scripts/build-docs.sh`,
  or read the chapters as Markdown under `book/src/`). Start here if you're new.
- **[Language reference](spec/language.md)** — the full syntax and semantics.
- **[Capabilities guide](spec/capabilities.md)** — the security model, for users.
- **[Standard library](spec/stdlib.md)** — the bundled modules, function-by-function.
- **[Examples](examples/)** — the exhaustive development corpus; see the
  [index](examples/README.md). A smaller public showcase is still being curated.
- **[Capability rights design](rfcs/capability-rights.md)** and
  **[package-manager design](rfcs/package-manager.md)** — the deeper design docs.
- **[Architecture](spec/architecture.md)** — how the compiler and backends fit
  together, and the parity discipline that keeps them honest.

Editor support: a [Zed extension](editors/zed) with tree-sitter highlighting and
`witchy lsp` diagnostics.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
