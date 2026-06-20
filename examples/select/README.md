# select

`chan.select` races two receivers and takes from whichever is ready (a tie
favours the first), yielding `Closed` once neither can deliver. Two producers
feed separate channels and one collector merges them — a single task reacting to
whichever source speaks next, which one receiver can't express.

**Shows:** `async`/`await`, channels (`Sender`/`Receiver`), `chan.spawn`,
`chan.select`, and `match` on the select outcome.

## Run

```sh
witchy run                                  # from this directory
witchy examples/select/src/select.witchy    # or by file, from the repo root
witchy parity examples/select/src/select.witchy
```
