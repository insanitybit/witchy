# Redis over a Witchy capability

`redis.connect` takes a borrowed launcher-provided `Net` and a Redis endpoint.
The returned `Redis` owns one connection. Its module exposes `ping`, `set`,
and `close`. Callers cannot extract the socket or send arbitrary commands
through this capability. The launcher's network grant controls which endpoint
can be dialed. Redis server permissions are a separate boundary.

Run a disposable local Redis server in one terminal.

```sh
redis-server --bind 127.0.0.1 --port 16379 --save '' --appendonly no
```

From the repository root in another terminal, using compiler and Wasmtime host
binaries from the same toolchain, select a trusted Lean 4.36.0 installation
for the compiler's proof checks:

```sh
export WITCHY_WASM_LEAN_ROOT=/path/to/lean-4.36.0
witchyc build examples/redis/main.wy -o /tmp/witchy-redis.wasm
witchy-wasmtime-host /tmp/witchy-redis.wasm --output --allocator --net=127.0.0.1:16379
redis-cli -p 16379 --raw GET 'witchy demo'
```

Expect `PONG` and `SET OK`. The CLI reads back a value with an embedded CRLF.
The client sends UTF-8 text keys and values as length-prefixed RESP arguments.
Close the connection after a command error, since a failed request or reply
can leave it between protocol frames. This example is scoped to plain TCP,
sequential commands, and status replies. It has no Redis authentication,
TLS, or GET decoder.

The example currently runs on Wasmtime. See the
[native development guide](../../docs/native-backend.md#endpoint-scoped-networking)
for the native endpoint-confinement requirements.


On macOS ARM64, build and run the same source natively:

```sh
witchyc build examples/redis/main.wy --target=macos-arm64 -o /tmp/witchy-redis
/tmp/witchy-redis --output --allocator --net=127.0.0.1:16379
```
