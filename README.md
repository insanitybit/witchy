# witchy

A capability-secure language where a program's authority is a **typed, auditable,
diffable, enforceable artifact**.

```witchy
// Provably cannot write.
fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir):
    // Full Dir narrows to Dir[Read].
    console.print(load(dir, "notes.txt"))
```

## Quick start

Build from source with a Rust toolchain, then run the hello-world example:

```sh
git clone https://github.com/insanitybit/witchy
cd witchy
cargo build --release
./target/release/witchy examples/hello/src/hello.witchy
```

### Private 0.1.0 installation

Witchy 0.1.0 is distributed only through this private GitHub repository. Access
requires an authorized GitHub account; there is no public or unauthenticated
download mirror. Authenticate with GitHub CLI, select the archive matching your
platform, and download it together with the checksum manifest:

```sh
gh auth login
gh release download v0.1.0 --repo insanitybit/witchy \
  --pattern witchy-0.1.0-aarch64-apple-darwin.tar.gz \
  --pattern SHA256SUMS --dir witchy-0.1.0-download
cd witchy-0.1.0-download
grep '  witchy-0.1.0-aarch64-apple-darwin.tar.gz$' SHA256SUMS | shasum -a 256 -c -
tar -xzf witchy-0.1.0-aarch64-apple-darwin.tar.gz
./witchy-0.1.0-aarch64-apple-darwin/bin/witchy --version
```

Use `x86_64-apple-darwin` for an Intel Mac or
`x86_64-unknown-linux-gnu` for x86-64 Linux. Linux users may replace
`shasum -a 256` with `sha256sum`. Verify the archive before extraction.

Every archive has exactly this layout, under its target-named root:

```text
witchy-0.1.0-<target>/
  bin/witchy
  README.md
  CHANGELOG.md
  LICENSE-MIT
  LICENSE-APACHE
```

The macOS binaries are not Apple-notarized or independently code-signed. An
authorized download made with `gh` normally runs directly; if local Gatekeeper
policy adds quarantine and blocks it, inspect the file and then remove that
attribute explicitly with `xattr -d com.apple.quarantine <root>/bin/witchy`.
This private-development workaround is not notarization.

## Why witchy?

Supply chain security is a serious problem. Packages have very good reasons for running
code during install time, yet the capability is typically all or nothing. Libraries in most
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
    let template = dir.read("report.tmpl")
    let notes    = http.body(http.get(net, "notes.internal", 443, "/today"))

    // Hand the untrusted rune the *data*, never the *authority*. `markdown.render`
    // takes only Strings — its signature can't receive `net` or `dir`, so nothing
    // it does (or anything it calls) can reach them. Only the String crosses back.
    let html = markdown.render(template, notes)

    console.print(html)
```

This is extremely hard to mess up. `markdown.render` never receives the capabilities — they
simply aren't parameters — so it couldn't dial out or read the disk even if a future version
tried. To get that authority it would have to change its signature (and your call), a visible,
reviewable change that `witchy caps` flags.

### Build-Time Execution, safe by default

The capability model extends out to build time execution. Build scripts are full `witchy` programs, but once
again we can reason about those in terms of their capabilities. A dependency `foo` that only
requires `Clock` gets compromised, now it asks for `Net` - that gets flagged!

```
$ witchy update
blocked: acme/logger 1.0.0 -> 1.1.0 widens the footprint
  + Net
run `witchy update --allow-cap Net` to accept, or pin the old version
```

No hidden "suddenly my dependency is pulling from ghostbin".

### Witchy's Package Registry - Coven

The witchy package registry, `coven`, holds packages named `runes`. This registry is
truly a "safe by default" system;

- Two modes: an anonymous local/demo mode, and trusted publishing — short-lived,
  single-use OIDC identity tokens with no long-lived API keys to compromise —
  enabled once at least one trusted issuer is configured (`--trust-issuer` /
  `--trust-issuer-jwks`). Trusted promotion derives its second-factor proof from
  the issuer-signed `amr` claim; a client-provided marker is never proof.
- Full support for package signing and TUF.
- Separation of states for "published" and "released" - CI can stage a package,
  but a distinct human identity with verified MFA must release it.
- Dependency cooldowns and default-deny for build-time execution are the default.

### Safety at every level

`witchy` aims to build security into every part of the stack.
- Capabilities are maximally granted at the wasm boundary, refined and attenuated as the program executes.
- The standard library includes lots of nuts and bolts so that you don't have to trust 3rd parties just to build
  a basic HTTP server or encode some data as json.
- Common footguns like "I want to limit my HTTP client to external IPs" are considered first class in the standard
  http library, safe patterns for common (but critical) use cases are intended to be trivial to express in `witchy`.
- Dependency cooldowns, Trusted Publishing, auditing, are all baked into the registry and the tooling.

Every piece of `witchy` is designed with security in mind.

## Language stability

Witchy 0.1.0 is a private, usable, compatibility-unstable checkpoint. The
installed CLI and release artifacts are exercised by executable release gates,
but 0.x releases may still make breaking language, standard-library, package,
artifact, and CLI changes without a deprecation period.

The parity, checked-heap, sandbox, and installed-artifact tests provide concrete
regression evidence; they are not a proof that the compiler or runtime has no
defects. In particular, current filesystem confinement is not race-free against
a concurrent local symlink swap. See [CHANGELOG.md](CHANGELOG.md) for the exact
0.1.0 surface and known limitations.

Portable `.wasm` artifacts remain untrusted guests run by a separately installed
Witchy host. A `trusted-exe`, by contrast, embeds the application, checked root
capability bindings, and the native Witchy runtime in one executable. Running it
trusts that complete artifact and its distributor; capabilities constrain
delegation inside the trusted application, not the trusted root's authority over
the user. Embedded digests detect corruption but do not authenticate a publisher.
For 0.1.0, publisher authentication is authorized access to the private GitHub
repository and release.

Grimoire/Coven integrated installation is not included in 0.1.0. Neither the
README nor the release claims proposed existential, `Dynamic`, lexical-extension,
or Grimoire semantics that are not independently implemented and tested.

## AI disclosure

`witchy` is a project for fun. It's vibecoded using different models. It's a way
for me to explore ideas in this space. If that's not for you, no problem!

For the most part, I like to focus on the safety of `witchy`, the ergonomics, etc. I then
defer virtually all of the "how it's built" and "how it executes" to an agent. I make some of
the major decisions like the use of wasm, priorities, syntax, the capabilities model, etc, but
for something like driving how an optimization actually applies during codegen I simply do not
care to be involved. I'm somewhat interested in that sort of work but my time is limited.

The way I build witchy is intentionally low effort, I have a day job. I primarily interact with witchy by iterating on an RFC document with an agent, sending the agent off to implement the RFC, and then reviewing the documentation of that work. I rarely interact with the witchy compiler code.

Some of the docs are handwritten, most are written by AI. RFCs are rarely hand edited, instead
I just give feedback and see what the AI comes up with until I accept it.

If anything changes with regards to AI usage (or anything that might feel meaningful to someone interested in witchy) I'll be sure to disclose that.

If you would like to contribute, please disclose any AI usage (with the model used),
`witchy` is a project that is accepting of AI written code but takes the position that
it *must* be open and honest about the use of AI.

## Run the docs

The guided way to learn witchy is **[The witchy Book](book/src/SUMMARY.md)** — which
_is_ a witchy program: a client-side [Glamour](projects/glamour/README.md) app that renders
every chapter and turns each code block into an editable, runnable cell (the ultimate
dogfood). Build the static bundle and serve it locally:

```sh
git clone https://github.com/insanitybit/witchy && cd witchy
cargo build --release            # the toolchain (compiler + native host)
./scripts/build-playground.sh    # the in-browser compiler behind the Run buttons
./scripts/build-docs.sh dist     # assemble the static bundle
python3 -m http.server -d dist 8000   # then open http://localhost:8000
```

That's a chapter-by-chapter tour from "hello" through capabilities, concurrency,
generators, and the package manager — every example is run and verified by the test
suite, so what you read is exactly what the language does. The bundle is a bag of
static files (no server), and you can always read the chapters as plain Markdown
straight from [`book/src/`](book/src/SUMMARY.md).

Want to _run code_ with zero install instead? The [playground](#playground)
compiles and runs your code in your browser. Or jump straight to
[Quick start](#quick-start) to build the `witchy` CLI.

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
- Concurrency: `async`/`await`, `spawn`, and first-class channels (the Go/CSP
  family), on a cooperative executor written in pure witchy.
- Structural equality, deep, on both backends.

## Package manager: `coven`

Packages ("runes") publish to a content-addressed, signed registry. The
distinguishing rule: **the registry and the client recompute every rune's
capability footprint from source — declared metadata is never trusted**, and
`witchy add`/`update` **block when a dependency's footprint widens** until you
explicitly approve. Two-phase publish (stage → verified-MFA promote), TUF-style
metadata against rollback/freeze, keyless OIDC trusted publishing, lockfiles,
vendoring.
Resolving and installing dependencies executes no dependency code; a dependency's
build step is default-deny, running only when you grant it explicitly — under
build-only capabilities, per-rune grants, and a post-build footprint check — and
published runes can vendor their generated source so consumers run no build step.

The package manager and registry are **self-hosted**: `projects/pm` and
`projects/coven` are witchy programs, exercised end-to-end by the test suite.

Try the whole lifecycle locally — server, trusted publish, 2FA promote,
verified consumption — with one command:

```sh
./scripts/local-registry-demo.sh
```

See [spec/local-registry.md](spec/local-registry.md) for the step-by-step
version, and [rfcs/package-manager.md](rfcs/package-manager.md) for the full
design and threat model.

## CLI

```
witchy [--net <host:port>]... <file.witchy>   run a program
                                              ([--signing-key <seed-file>] grants the root Secret)
witchy check    <file.witchy>                 check + verify compiled acceptance without running
witchy parity   <file.witchy>                 run on both backends, confirm identical output
                                              (a verify-the-compiler tool, not a workflow step)
witchy sandbox [--dir <root>] [--net <addr>]... <file.witchy> [args...]
                                              compile and run in a VM granted exactly its footprint
witchy emit-wat <file.witchy>                 print the compiled WebAssembly text
witchy caps     <file.witchy>                 report the capability footprint (runtime + build axes)
witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened on either axis
witchy build-step <file.witchy>               run a rune's build step under confined grants
                                              ([--out <dir>] [--read <dir>] [--env K]... [--exec tool]...)
witchy --release build --target trusted-exe   build one trusted, self-contained native application
                                              (running it trusts app + embedded runtime + distributor)
witchy test [--integration] [--dir <root>]... [--net <addr>]... <file.witchy|dir>
                                              run zero-grant unit tests or explicitly granted integration tests
witchy fmt [--check] <file.witchy>            reformat (--check: verify only)
witchy doc      <file.witchy>                 extract Markdown API docs
witchy lsp                                    run the language server
witchy --bench                                compare interpreter vs compiled execution
witchy new/add/build/run/publish/...          package-manager commands
```

## Playground

witchy's compiler — front end, type checker, and the WIR → wasm backend —
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
with the language reference's examples preloaded.

## Learn more

- **[The witchy Book](book/src/SUMMARY.md)** — the guided, chapter-by-chapter
  introduction (a client-side Glamour app: build it with `./scripts/build-docs.sh`,
  or read the chapters as Markdown under `book/src/`). Start here if you're new.
- **[Language reference](spec/language.md)** — the full syntax and semantics.
- **[Capabilities guide](spec/capabilities.md)** — the security model, for users.
- **[Standard library](spec/stdlib.md)** — the bundled modules, function-by-function.
- **[Examples](examples/)** — 100+ programs from `hello` to a self-hosted
  package registry; see the [index](examples/README.md).
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
