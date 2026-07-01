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

From the repository root, run the example by file:

```sh
witchy examples/coven_check/src/coven_check.witchy
```

The example reads fixture files under `examples/data`, so run it with the
repository root as the current directory. If you are using an uninstalled debug
binary, substitute `target/debug/witchy` for `witchy`.

The checked fixture is intentionally under-declared: it admits `Net[Connect]`,
while the sample rune demands full `Net`. The command should print
`UNDER-DECLARED: code demands Net not admitted by [capabilities]` and exit
non-zero. If you change the fixture to admit the full source-derived footprint,
the same checker prints `OK: manifest admits everything the code demands` and
exits with status `0`.
