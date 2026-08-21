# zip

Pairs lists two ways: `list.enumerate` pairs each element with its index, and
`list.zip` pairs two lists element-wise. Both yield lists of tuples, which the
`for` loops destructure. Data-only apart from `main` (root footprint: `Console`); identical interpreted and compiled.

**Shows:** `list.enumerate`/`list.zip`, tuple destructuring in `for` loops, and
string interpolation.

## Run

```sh
witchy run                            # from this directory
witchy examples/zip/src/zip.witchy    # or by file, from the repo root
```
