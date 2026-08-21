# serve_hello

A capability-secure web server. The ordinary `fn(Request) -> Response` handlers
are closed over request data and receive no `Net`/`Dir`/`Console`, so they cannot
touch the network, filesystem, or console as written. Only `main` holds the
`Net`, and it hands it to `serve` to listen, never to a handler. Use `pure fn`
when an API needs a checked effect-free callable contract rather than this
example's empty handler footprint.

**Shows:** the `server` router, `get`/`post` routes, path params, an inline
handler closure, and capability-confined request handling.

## Run

```sh
witchy run --net 127.0.0.1:8080                                    # from this directory
witchy --net 127.0.0.1:8080 examples/serve_hello/src/serve_hello.witchy
```
