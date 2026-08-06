# aliases

Type aliases name a type for clarity without creating a new, incompatible one:
`type Celsius = Int` *is* `Int`, just more legible in signatures. Aliases may
stand for compound types and may chain; they are expanded before type-checking
and code generation, so they cost nothing at runtime and behave identically
interpreted, compiled, and inside the capability sandbox.

**Shows:** `type` aliases (scalar and `List(...)`), `for` loops, `pub` functions
across modules, and the `Console` capability.

## Run

```sh
witchy run                                  # from this directory
witchy examples/aliases/src/aliases.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/aliases
```
