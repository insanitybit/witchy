# monitor

A generator feeding a running computation: `readings` is an infinite `gen fn`
(a pseudo-random stream), bounded by `iter.take`, collected into a list, then
folded into a high-water mark as each sample is printed.

**Shows:** `gen fn` / `yield`, the lazy `iter` library (`take`, `collect`),
string interpolation, and the `Console` capability.

## Run

```sh
witchy run                                    # from this directory
witchy examples/monitor/src/monitor.witchy    # or by file, from the repo root
```
