# serve_hello

A capability-secure web server. Handlers are pure `fn(Request) -> Response` —
they hold no `Net`/`Dir`/`Console`, so a handler literally cannot touch the
network, filesystem, or console. Only `main` holds the `Net`, and it hands it to
`serve` to listen, never to a handler. The router, path params, and middleware
are axum-flavored; the capability sandbox is pure witchy.

**Shows:** the `server` router, `get`/`post` routes, path params, an inline
handler closure, and capability-confined request handling.

## Run

```sh
witchy run --net 127.0.0.1:8080                                    # from this directory
witchy --net 127.0.0.1:8080 examples/serve_hello/src/serve_hello.witchy
```
