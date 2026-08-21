# serve_api

A small JSON API with axum-style routing and a tower-style logging middleware.
Handlers are ordinary delegated callables: the `logging` layer can print because
`main` captures a `Console` into it, while the closed `home` and `greet`
implementations only build responses from request data. `serve` keeps the
listening `Net`; an API that must enforce effect-free handlers should require
`pure fn` explicitly.

**Shows:** the `server` router, path params, middleware layers as
`fn(Handler) -> Handler` closures, capability capture as dependency injection,
and the `json` module.

## Run

```sh
witchy run --net 127.0.0.1:8080                                # from this directory
witchy --net 127.0.0.1:8080 examples/serve_api/src/serve_api.witchy
```
