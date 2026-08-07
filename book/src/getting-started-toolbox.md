# The Toolbox

The `witchy` binary provides the toolchain. The commands below follow a typical
first use.

## Running and checking

```sh
witchy program.witchy            # compile and run
witchy check program.witchy      # check + verify compiled acceptance, don't run
witchy fmt program.witchy        # reformat in place (canonical layout)
witchy fmt --check program.witchy  # verify formatting (for CI); exit 1 if not
```

`check` catches type errors, capability/exhaustiveness errors, performance-mode
violations, and compiled-backend acceptance failures without running the program.
`fmt` is the canonical formatter; it preserves your comments and is idempotent,
so there's one true layout and no style arguments.

Know that `fmt` canonicalizes *forms*, not just whitespace: a
pre-migration rendering call prints back as the interpolation `"a ${x}"`,
escaped quotes inside `${...}` become bare, nullary constructors lose their
`()`, single-statement match arms go inline, and `if let` survives as
`if let`. Every rewrite is meaning-preserving - the formatter won't emit
anything that doesn't parse back to the same program - but the bytes may
change more than you expect the first time you run it.

## Seeing and enforcing authority

```sh
witchy caps program.witchy                  # the capability footprint (runtime + build)
witchy caps-diff old.witchy new.witchy      # exit 2 if authority widened on either axis
witchy which split                          # where a method lives: String.split(sep), with its doc

witchy sandbox program.witchy               # run confined in the WASM VM
witchy grants-check program.witchy app.grants.toml   # check a grant doc against the footprint
witchy build-step gen.witchy --out gen/     # run a build step under confined grants
```

`caps` prints what the program is allowed to do, per function and per right -
and, when a rune ships a `build` step, what that step may do at *build time*,
on its own axis. `caps-diff` is the CI gate against silent authority growth on
either axis. `sandbox` compiles to WebAssembly and runs the program in a VM
granted exactly its footprint:

- `--dir <root>` backs a `Dir`,
- `--file <path>` backs a single `File` parameter (the i-th `File` ← the i-th `--file`),
- `--net <host:port>` allowlists a network address,
- `--fetch <scheme://host:port>` allowlists an HTTP origin for `Fetch`,
- `--grants <file>` reads the whole grant from a reviewable TOML document.

`grants-check` validates such a document against the program's computed footprint
without running it - exit 2 if the grant withholds authority the code needs (or
warns if it over-grants). The capabilities chapters use these commands heavily.

## Compiling

```sh
witchy emit-wat program.witchy      # print the generated WebAssembly text
witchy expand program.witchy        # print source after comptime/tag expansion
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
witchy tree .             # dependency tree with each rune's authority
witchy audit src/app.witchy  # recompute one source file's authority
```

`witchy --help` lists these commands. The rest of the book introduces them in
context.
