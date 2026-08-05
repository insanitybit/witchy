# carried_state

A **sealed record capability** (RFC-0011): authority *plus* carried policy in one
unforgeable value.

`Postgres` wraps a `Net` confined to one database host — the **hard, audited**
authority — and carries a `table` filter beside it — a **soft** policy the library
enforces in its own operations:

```witchy
capability Postgres:
    net: Net[Connect, Tcp]
    table: String
```

Because it is declared `capability`, it is **sealed**: only this module can mint
(`connect`), refine (`use_table`), or destructure one, and its fields are private —
you reach them with `match`, never `.field`, so an alias can never leak the
underlying `Net` past the table policy.

The footprint analyzer sees straight through the record. `witchy caps` reports
`connect`, `use_table`, and `count_rows` as `Net[Connect, Tcp]` — the carried
`String` adds no authority, so nothing hides:

```sh
witchy caps examples/carried_state/src/carried_state.witchy
```

The two enforcement tiers in one type:

- **Hard** — the `Net` is confined to the DB host; the runtime will not dial
  elsewhere, and the footprint proves it.
- **Soft** — `count_rows` refuses any table but the one the handle carries. The
  network cannot enforce "table = users"; the *library* does, in one reviewable
  place. Forgery can't widen it (sealed), but a buggy library could — that is the
  honest trade of the soft tier.

Run it:

```sh
witchy examples/carried_state/src/carried_state.witchy
```
