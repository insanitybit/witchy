# serve_static

Serving static files, and a vivid capability story. The file handler captures a
`Dir` scoped to `./examples/data`, so it can read files there but cannot reach
the network (it has no `Net`) or any other directory — a `Dir` is confined to
its subtree, and `../secret` and symlinks can't escape it. `serve` holds the
`Net` to listen and never hands the `Dir` or `Net` to a route that didn't
capture it. `exists` keeps the handler total: a missing file is a 404, not a
crash.

**Shows:** the `server` router, wildcard path params, the `Dir` capability and
`subdir` scoping, capability capture into a handler closure, and `not_found`.

## Run

```sh
witchy run --net 127.0.0.1:8080                                      # from this directory
witchy --net 127.0.0.1:8080 examples/serve_static/src/serve_static.witchy
```
