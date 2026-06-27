# file_capability

The **`File`** capability (RFC-0012): authority to *one file* — the leaf of the
`Dir`/`File` hierarchy. A function that needs a single file no longer has to be
handed a whole `Dir`.

A `Dir` navigates down to a single file, and the leaf is read/written directly
(no path argument):

```witchy
let config = dir.read_file("config.txt")     // File[Read]  (must exist)
let log    = dir.write_file("run.log")      // File[Write] (need not exist)
print(console, read(config))
write(log, "started")
```

`load_config` here takes `File[Read]`, the least authority that reads one file —
so it provably cannot see any other file. `witchy caps` reports it as exactly
`File[Read]`, not `Dir`. Navigation keeps the Dir's `..`/absolute confinement:
`dir.read_file("../x")` is rejected on both backends.

A `File` can also be granted to `main` **directly** (`--file <path>`) for the
least-authority single-file case — `main(config: File[Read])` with no `Dir` at
all. This example navigates one from a `Dir` so it is self-contained.

Run it (reads this example's `data/config.txt`):

```sh
witchy examples/file_capability/src/file_capability.witchy
```
