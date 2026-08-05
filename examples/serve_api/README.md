# serve_api

A small JSON API with axum-style routing and a tower-style logging middleware.
The part no other framework has: handlers are pure by construction. The
`logging` layer can print only because `main` captured a `Console` into it; a
plain handler captures nothing, so it structurally cannot log, fetch a URL, or
read a file. `serve` holds the `Net`; handlers never do.

**Shows:** the `server` router, path params, middleware layers as
`fn(Handler) -> Handler` closures, capability capture as dependency injection,
and the `json` module.

## Run

```sh
witchy run --net 127.0.0.1:8080                                # from this directory
witchy --net 127.0.0.1:8080 examples/serve_api/src/serve_api.witchy
```
