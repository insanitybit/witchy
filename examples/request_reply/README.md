# request_reply

Request/reply over a reply channel. The client makes its own one-shot channel
and sends the `Sender` end inside the request; the server replies on whatever
channel the request carried. No task indices, no hardcoded addresses — the reply
path is a first-class value the client chose. The server uses `chan.serve` to
fold over its inbox, and `main` runs it beside the client in one `chan.scope`.

**Shows:** `async`/`await`, `chan.channel` / `scope` / `send` / `recv` / `serve`,
sending a `Sender(Msg)` inside a message, sum-type messages, and
`match`.

## Run

```sh
witchy run                                                  # from this directory
witchy examples/request_reply/src/request_reply.witchy      # by file, from repo root
witchy parity examples/request_reply/src/request_reply.witchy
```
