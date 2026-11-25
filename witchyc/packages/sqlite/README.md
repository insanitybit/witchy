# SQLite for Witchy

`sqlite` exposes one bounded SQLite transaction on an explicit `Sqlite`
grant. The Wasm host performs the transaction and enforces access to the
database and its tables. The package cannot open a database or grant
itself access.

Build the versioned library payload with `grimoire build packages/sqlite`
from the Witchy checkout. A Grimoire release with SQLite launch support
can run an application using this grant document:

```toml
[sqlite]
db = { file = "data/items.db", read = ["items"] }
```

Here `db` is the name of the application's `Sqlite` parameter. The
database must already exist in a private directory inside the project.
Select the grant document under `[grants]` in the application's manifest
and run `grimoire run` from the project directory. The direct Wasmtime
host commands below use the same grant.

Add the package to the application's package list. For example, from
`examples/sqlite-read.wy` in this checkout:

```text
sqlite 0.2.0: ../packages/sqlite/sqlite.wy ; requires
```

In the application, use `import "@sqlite"` and give `main` a
`db: Sqlite` capability. Grant only the required table operations in the
run configuration. For the wire format and grant syntax, see the
[SQLite capability](../../docs/wasm-capabilities.md#a-scoped-sqlite-transaction).

## Transaction results

Use `db.transact_checked(plan, bindings, blob, reply)` with a caller-owned
mutable byte slice. Call it once per grant. `Ok(length)` means the transaction
committed and the nonempty result occupies `reply[1..1 + length]`. If it
returns `Err(AssertionFailed(length))`, that range holds the failed step's
label and the transaction rolls back. `Unavailable` covers refusals such as
a denied table operation or a result that exceeds its reply budget.
`NoReply` means the host returned no bytes, for example with an empty reply
buffer. `InvalidReply` means the host reply did not match the format.

The original `transact` method still returns the raw byte count, including
the status byte. The package does not allocate or add any grant beyond the
supplied `Sqlite`.

## Run the read-only example

From the Witchy checkout, set `scratch` to a private directory whose
absolute path contains no symlinks. Create the example database before
running the guest.

```sh
chmod 0700 "$scratch"
sqlite3 "$scratch/items.db" \
  "CREATE TABLE items(name TEXT PRIMARY KEY, value TEXT); INSERT INTO items VALUES ('sample', 'hello SQLite');"
witchyc wasm-caps --policy examples/sqlite-read.policy examples/sqlite-read.wy
witchyc build examples/sqlite-read.wy -o "$scratch/read.wasm"
witchy-wasmtime-host "$scratch/read.wasm" --allocator --output \
  --sqlite="$scratch/items.db" --sqlite-read=items
```
