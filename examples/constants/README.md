# constants

Top-level constants: `let NAME = EXPR` at module scope names a value once and
reuses it everywhere, and a constant may build on earlier constants
(`SECONDS_PER_HOUR = SECONDS_PER_MINUTE * MINUTES_PER_HOUR`). They are inlined at
their use sites before code generation, so they cost nothing at runtime. The data-only
`to_seconds` (`pub`) folds the units together; only `main` touches the `Console`.

**Shows:** module-scope constants, constants defined in terms of other constants,
`pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/constants/src/constants.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/constants
```
