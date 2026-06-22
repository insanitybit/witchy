# The Toolbox

The `witchy` binary is the whole toolchain. You'll reach for a few commands
constantly; here they are, roughly in the order you meet them.

## Running and checking

```sh
witchy program.witchy            # compile and run
witchy check program.witchy      # type-check only, don't run (also prints
                                 # performance notes, e.g. copy-path loops)
witchy fmt program.witchy        # reformat in place (canonical layout)
witchy fmt --check program.witchy  # verify formatting (for CI); exit 1 if not
```

`check` is fast and catches everything the type system can — including
capability and exhaustiveness errors — without running anything. `fmt` is the
canonical formatter; it preserves your comments and is idempotent, so there's
one true layout and no style arguments.

Know that `fmt` canonicalizes *forms*, not just whitespace: a
pre-migration rendering call prints back as the interpolation `"a ${x}"`,
escaped quotes inside `${...}` become bare, nullary constructors lose their
`()`, single-statement match arms go inline, and `if let` survives as
`if let`. Every rewrite is meaning-preserving — the formatter refuses to emit
anything that doesn't parse back to the same program — but the bytes may
change more than you expect the first time you run it.

## The parity harness (you'll likely never need it)

```sh
witchy parity program.witchy
```

This runs a program on *both* the interpreter and the compiled WebAssembly
backend and confirms identical output — including identical failure. It is
how **witchy verifies witchy**: the project's CI sweeps every example and
test through it to enforce the zero-silent-divergence promise. As a witchy
*user* you rely on that promise rather than re-checking it — the compiler
already stops you loudly if you reach an edge the compiled tier can't
express. Reach for `parity` only if you're hacking on witchy itself (or
filing a compiler bug — its output is the perfect reproduction).

## Seeing and enforcing authority

```sh
witchy caps program.witchy                  # the capability footprint (runtime + build)
witchy caps-diff old.witchy new.witchy      # exit 2 if authority widened on either axis
witchy which split                          # where a function lives: string.split(s, sep), with its doc

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
```

## Tests and docs

```sh
witchy test suite.witchy            # run `test_*` functions in a file or directory
witchy doc module.witchy            # extract Markdown API docs from doc comments
```

## Packages

```sh
witchy new my-rune        # scaffold a new package ("rune")
witchy new my-lib --lib   # scaffold a library: pub fns, no main
witchy add acme/logger    # add a dependency (gated on capability widening)
witchy build              # resolve, link, and type-check the project
witchy run                # build and run
witchy audit              # the whole dependency tree's authority
```

You don't need to memorize these — `witchy --help` lists them, and each is
introduced where the book uses it. Now let's learn the language itself.
