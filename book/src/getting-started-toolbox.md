# The Toolbox

The `witchy` binary is the whole toolchain. You'll reach for a few commands
constantly; here they are, roughly in the order you meet them.

## Running and checking

```sh
witchy program.witchy            # run on the interpreter
witchy check program.witchy      # type-check only, don't run (also prints
                                 # performance notes, e.g. copy-path loops)
witchy fmt program.witchy        # reformat in place (canonical layout)
witchy fmt --check program.witchy  # verify formatting (for CI); exit 1 if not
```

`check` is fast and catches everything the type system can — including
capability and exhaustiveness errors — without running anything. `fmt` is the
canonical formatter; it preserves your comments and is idempotent, so there's
one true layout and no style arguments.

## The parity check

```sh
witchy parity program.witchy
```

This runs your program on *both* the interpreter and the compiled WebAssembly
backend and confirms they produce identical output — including identical
failure (an out-of-bounds index traps on both, an unparseable number errors on
both). It's how the language keeps its promise that the sandbox runs the same
program you developed against. When you write witchy, `parity` is your "did I
stay inside the portable language?" check.

## Seeing and enforcing authority

```sh
witchy caps program.witchy                  # the capability footprint (runtime + build)
witchy caps-diff old.witchy new.witchy      # exit 2 if authority widened on either axis
witchy sandbox program.witchy               # run confined in the WASM VM
witchy build-step gen.witchy --out gen/     # run a build step under confined grants
```

`caps` prints what the program is allowed to do, per function and per right —
and, when a rune ships a `build` step, what that step may do at *build time*,
on its own axis. `caps-diff` is the CI gate against silent privilege growth on
either axis. `sandbox` compiles to WebAssembly and runs the program in a VM
granted exactly its footprint — pass `--dir <root>` to back a `Dir`,
`--net <host:port>` to allowlist a network address. We'll use these heavily in
the capabilities chapter.

## Compiling

```sh
witchy emit-wat program.witchy      # print the generated WebAssembly text
witchy emit-rust program.witchy     # print the Rust transpilation
```

## Tests and docs

```sh
witchy test suite.witchy            # run `test_*` functions in a file or directory
witchy doc module.witchy            # extract Markdown API docs from doc comments
```

## Packages

```sh
witchy new my-rune        # scaffold a new package ("rune")
witchy add acme/logger    # add a dependency (gated on capability widening)
witchy build              # resolve, link, and type-check the project
witchy run                # build and run
witchy audit              # the whole dependency tree's authority
```

You don't need to memorize these — `witchy --help` lists them, and each is
introduced where the book uses it. Now let's learn the language itself.
