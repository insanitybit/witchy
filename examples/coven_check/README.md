# coven_check

A manifest-vs-code consistency check, written entirely in witchy. It reads a
rune's `witchy.toml` and its source with a read-only `Dir` capability, asks the
compiler what capabilities the code actually demands (`compiler.footprint`), and
verifies the manifest's `[capabilities]` admits them rights-precisely — a declared
`Net[Connect]` does not cover a demanded full `Net`. This is the package manager's
`check_declared`, self-hosted. It exits non-zero when the manifest is
under-declared.

**Shows:** the `Dir[Read]` capability, `compiler.footprint`, std `toml`/`json`/
`rights`, `match`/`if let`, `Option`, and an `Int` exit code from `main`.

## Run

```sh
witchy run                                          # from this directory
witchy examples/coven_check/src/coven_check.witchy  # or by file, from the repo root
```
