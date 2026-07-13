# serve_static

Serving fixed static content with a minimal capability boundary. The route
handler is a bare top-level function, so it captures no ambient authority:
`serve` holds the `Net` to listen, while the handler has exactly the request
value it is passed. Capability-carrying route closures are intentionally
rejected until RFC-0005's typed closure environments land.

**Shows:** the `server` router, a top-level route handler, and a listener
capability that never leaks into request handling.

## Run

```sh
witchy run --net 127.0.0.1:8080                                      # from this directory
witchy --net 127.0.0.1:8080 examples/serve_static/src/serve_static.witchy
```
