# witchy

Every mainstream language hands your code ambient authority. A function you call
can read `~/.ssh`, open a socket, or shell out, and its signature won't tell you.
You find out by auditing the implementation, and then everything it calls.

witchy is an experimental capability-secure language that doesn't work that way.
A function can only touch the outside world through capability values it's
explicitly handed - so what a program *can do* is visible in its types,
inspectable from its artifacts, and enforceable at the host boundary.

```witchy
// This helper receives read authority, not write authority.
fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir):
    // Full Dir narrows to Dir[Read].
    console.print(load(dir, "notes.txt"))
```

A web API where handlers are pure by construction - `serve` holds the `Net`,
and a handler that captures no capabilities structurally can't log, fetch a
URL, or read a file, even if a dependency wrote it:

```witchy
from http import Request, Response
from json import Json
from server import Router

fn greet(req: Request) -> Response:
    server.json_value(200, JsonObject([("hello", JsonString(server.param_or(req, "name", "world")))]))

fn main(console: Console, net: Net):
    let app = server.router().get("/hello/:name", greet)
    console.print("serving on 127.0.0.1:8080")
    server.serve(net, "127.0.0.1:8080", app)
```

An HTTP client holds an origin-scoped `Fetch`, never the raw network:

```witchy
import http
from http import Response

fn main(console: Console, fetch: Fetch):
    match http.get(fetch, "https://example.com/"):
        Response(status, headers, body) -> console.print("status ${status}: ${body.length()} bytes")
```

And the toolchain makes authority a checkable fact rather than a claim:

```sh
$ witchy caps api.witchy          # what can this program touch?
  main   Console, Net
  total  Console, Net
$ witchy --net 127.0.0.1:8080 api.witchy      # grant exactly that, run it
$ witchy sandbox api.witchy                   # or run it confined in a WASM VM -
                                              # ungranted authority fails closed
```

## Quick start

Prebuilt `witchy` binaries for x86-64 Linux, x86-64 macOS, and arm64 macOS are
on the [releases page](https://github.com/insanitybit/witchy/releases); verify
`SHA256SUMS`, then put `bin/witchy` on your `PATH`. Or build from source:

```sh
cargo build --release
witchy=./target/release/witchy
$witchy examples/hello/src/hello.witchy
$witchy parity examples/hello/src/hello.witchy   # identical output on both backends
```

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

`Int`/`Float`/`Bool`/`String`, native `Duration` literals (`30s`, `2hr`),
`List`/`Dict`/tuples/records/ADTs, `Option`/`Result` with `?`, traits with
`where` bounds, and Hylo-style parameter conventions (`let`/`var`/`own`, with
use-after-move as a compile error). Structural equality is deep on both
backends. Async/channels, generators, and comptime exist as experimental
surfaces.

## Why capabilities?

A witchy function can't exercise host authority absent from its typed inputs,
and `witchy caps` / `caps-diff` / `grants-check` / `sandbox` make that footprint
inspectable and enforceable.

That's a bounded guarantee, and the bound is worth being precise about. You
still trust the compiler, the runtime, whichever host bindings you link, and
whoever shipped you the binary. What you stop having to trust is the body of
every function you call. The [capabilities guide](spec/capabilities.md) states
the exact model and its limits.

## Status

Pre-1.0 and compatibility-unstable; anything may break without a deprecation
period. The dependable path is deliberately small - language fundamentals,
capability inspection, check/format/test, interpreter-versus-WASM parity,
portable WASM sandboxing, and self-contained
[`trusted-exe`](rfcs/0092-trusted-application-executables.md) builds. Everything
else (runes + the Coven registry, the Glamour frontend, the in-browser
playground, editor tooling) is experimental dogfood.
[PRODUCT-STATUS.md](PRODUCT-STATUS.md) is the evidence-backed boundary.

witchy is developed extensively with AI assistance; human judgment owns the
language, capability-model, and product decisions. What determines supported
behavior is executable evidence - the parity, sandbox, and artifact test suites -
rather than who or what wrote the code. Contributions should disclose material
AI use.

## Learn more

- **[The witchy Book](book/src/SUMMARY.md)** - the guided introduction; start here.
- **[Language reference](spec/language.md)** - full syntax and semantics.
- **[Capabilities guide](spec/capabilities.md)** - the security model.
- **[Standard library](spec/stdlib.md)** - the bundled modules.
- **[Examples](examples/README.md)** - runnable programs for every concept,
  including the [web API](examples/serve_api/src/serve_api.witchy) above.
- **[Architecture](spec/architecture.md)** - the twin backends and the parity
  discipline that keeps them honest.

Every ` ```witchy ` block in the book, spec, and this README is a complete
program validated by the test suite; the runnable ones execute on both
backends. Editor support: a [Zed extension](editors/zed) with tree-sitter
highlighting and `witchy lsp`.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option. Unless you explicitly state otherwise, contributions are dual licensed
as above.
