# display

The `Show` trait renders any type as a String — including your own types, which the
built-in `to_string` cannot. The example implements `Show` for a `Coord` type and
prints whole lists with `show.say`, whose blanket `impl Show for List(a) where a:
Show` renders each element through its own `Show`, so it works for lists of user
types too and produces the same output whether interpreted or compiled.

**Shows:** traits and `impl`, `match` on a custom type, generics over a trait
bound, and the std `show` module.

## Run

```sh
witchy run                                    # from this directory
witchy examples/display/src/display.witchy    # or by file, from the repo root
```
