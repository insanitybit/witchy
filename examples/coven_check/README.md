# coven_check

A manifest-vs-code consistency check, written entirely in Witchy. It reads a
rune's `witchy.toml` and its source with a read-only `Dir` capability, asks the
compiler what capabilities the code actually demands (`compiler.footprint`), and
verifies the manifest's `[capabilities]` admits them rights-precisely — a declared
`Net[Connect]` does not cover a demanded full `Net`. This is the package manager's
`check_declared`, self-hosted. It exits non-zero when the manifest is
under-declared.

**Shows:** the `Dir[Read]` capability, `compiler.footprint`, std `toml`/`json`/
`rights`, `match`/`if let`, `Option`, and an `Int` exit code from `main`.

## Quickstart

Run it from the repository root so the read-only directory grant can see the
sample inputs in `examples/data/`:

```sh
witchy examples/coven_check/src/coven_check.witchy
```

A successful run prints `OK: manifest admits everything the code demands`. To
try the under-declared path, edit a copy of `examples/data/rune_manifest.toml` to
remove one of the capabilities demanded by `examples/data/sample_rune_v2.witchy`,
then update the paths near the top of `src/coven_check.witchy` to point at the
copy.

You can also run the rune from this directory if your local Witchy build grants
the repository root as the `Dir[Read]` capability:

```sh
witchy run
```
