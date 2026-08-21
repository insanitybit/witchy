# math_demo

Calls the bundled `math` standard library — `max`, `abs`, `clamp`, `pow`, and
`gcd`. These operations are data-only, and importing `math` adds no root
capability demand. That footprint fact is separate from a checked `pure fn`
contract.

**Shows:** importing a std module, calling module-qualified functions, and the
`Console` capability.

## Run

```sh
witchy run                                        # from this directory
witchy examples/math_demo/src/math_demo.witchy    # or by file, from the repo root
```
