# Web APIs and Services

Witchy's web server framework (`std/server`) combines the routing model of
axum with Tower-style middleware, backed by value handlers and explicit
capability delegation.

In Witchy, HTTP handlers have the ordinary type `fn(Request) -> Response`. That
type is opaque delegated behavior and may be effectful: a closure can capture a
narrowed logger, store, or outbound client supplied by its creator. The
listening socket is held by `server.serve`, which never passes that specific
network capability to handlers. This confines listener authority; it does not
make the ordinary handler type a purity contract. The checked `pure fn`
qualifier is the explicit effect-free contract.

## Routing and JSON responses

Use `server.router()` to declare routes. Handlers extract path parameters using
`server.param` or `server.param_or`, and respond with JSON using `server.send`
and anonymous struct literals:

```witchy
from http import Request, Response
from server import Router

fn greet(req: Request) -> Response:
    let name = server.param_or(req, "name", "world")
    server.send(200, .{greeting: "Hello, " + name + "!"})

fn main(console: Console):
    let app = server.router().get("/hello/:name", greet)
    let req = Request("GET", "/hello/witchy", [("name", "witchy")], [], [], "")
    let res = server.handle(app, req)
    match res:
        Response(code, _headers, body) ->
            console.print("code: ${code}, body: ${body}")
```

```text
code: 200, body: {"greeting":"Hello, witchy!"}
```

`server.handle(app, req)` executes an in-memory request against the router, making
integration tests fast and deterministic without binding a TCP socket.

## Middleware and dependency injection

Middleware layers wrap handlers (`fn(Handler) -> Handler`). They have no ambient
access to authority, so any required capability (such as `Console` for logging
or `Dir` for storage) is explicitly passed into the middleware factory and then
delegated through the resulting closure:

```witchy
from http import Request, Response
from server import Router

fn logging(console: Console) -> fn(fn(Request) -> Response) -> fn(Request) -> Response:
    fn(next: fn(Request) -> Response):
        fn(req: Request):
            console.print("request: " + server.method(req) + " " + server.path(req))
            next(req)

fn health(_req: Request) -> Response:
    server.send(200, .{status: "healthy"})

fn main(console: Console):
    let app = server.router()
        .get("/health", health)
        .layer(logging(console))

    let req = Request("GET", "/health", [], [], [], "")
    let _res = server.handle(app, req)
```

```text
request: GET /health
```

## Running the server on the network

To listen on a real network interface, pass the `Net` capability and port to
`server.serve`:

```sh
from http import Request, Response
from server import Router

fn greet(req: Request) -> Response:
    server.send(200, .{hello: server.param_or(req, "name", "world")})

fn main(console: Console, net: Net):
    let app = server.router().get("/hello/:name", greet)
    console.print("serving on 127.0.0.1:8080")
    server.serve(net, "127.0.0.1:8080", app)
```

Run with the capability grant:
```sh
$ witchy --net 127.0.0.1:8080 server.witchy
```
