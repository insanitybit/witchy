# firewall

Capability firewalls: `without` and `retain` carve a region of a function where
only the capabilities you name remain in scope. It is a compile-time guarantee —
the type checker hides the bindings while every backend runs the block normally —
that a slice of code does no more than the authority it asks for. Here a `without
console:` block filters names with the pure `is_valid` (no capabilities, so
provably no I/O), and a fully sealed `retain:` block sums their lengths; each
block's value crosses the firewall while the dropped capabilities do not.

**Shows:** `without`/`retain` capability firewalls, blocks as values, closures
passed to `list.filter`, pure (capability-free) functions, and in-rune `test_*`
functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/firewall/src/firewall.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/firewall
```
