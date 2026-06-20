# mutate

Mutable value semantics (Hylo-style): a `var` is freely mutable, and a `var`
parameter lets a function mutate the caller's variable in place — no pointers.
Because values are independent there is no aliasing to reason about; `let`
bindings stay immutable.

**Shows:** `var` bindings, `var` parameters (in-place mutation by value), and the
`Console` capability.

## Run

```sh
witchy run                                  # from this directory
witchy examples/mutate/src/mutate.witchy    # or by file, from the repo root
```
