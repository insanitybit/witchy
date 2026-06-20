# display

The `Show` trait renders any type as a String — including your own types, which the
built-in `to_string` cannot. The example implements `Show` for a `Coord` type and
formats whole lists through `show.show_list`, so it works for lists of user types
too and produces the same output whether interpreted or compiled.

**Shows:** traits and `impl`, `match` on a custom type, generics over a trait
bound, and the std `show` module.

## Run

```sh
witchy run                                    # from this directory
witchy examples/display/src/display.witchy    # or by file, from the repo root
```
