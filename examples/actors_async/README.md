# actors_async

Independent server tasks, each owning its OWN channel: a `logger` prints what it
receives, a `forwarder` relays to the logger, and a `driver` sends to both. Each
channel is a separate first-class value passed where it's needed.

**Shows:** `async fn`/`await`, `chan.channel`/`spawn`/`serve`/`consume`/`send`/`join`,
typed `Sender`/`Receiver`, and the `Console` capability threaded into a task.

## Run

```sh
witchy run                                              # from this directory
witchy examples/actors_async/src/actors_async.witchy    # or by file, from the repo root
```
