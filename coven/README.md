# Coven

Coven is the Witchy package registry. Package authors and consumers use
`grimoire`. Production distribution serves a signed TUF repository and the
Glamour catalog from a Witchy HTTPS mirror. CI and maintainer review use
separate Witchy HTTPS origins.

- [Deploy the public mirror](deploy/witchy2/README.md)
- [Deploy private CI staging and maintainer review](deploy/witchy2/PRIVATE.md)
- [Authorize, publish, and fetch TUF releases](witchy2/tuf.md)

Witchy implements TLS and HTTP, validates GitHub OIDC tokens, and authorizes
release and staging transactions. SQLite stores private staging state. The
service entry points use the Wasmtime host for scoped sockets, cryptographic
operations, and SQLite access. The Python launchers provision those grants
and supervise the workers.

## Local registry

The loopback registry is available for fixtures and local development.

```sh
coven serve --store ./store --port 8787
```

The server creates a private storage directory if needed. Its HTTP endpoint is `http://127.0.0.1:8787`. Consumers configure that URL and their trust anchors. The operator bootstraps the first snapshot against the private store. Subsequent snapshots can be signed from the operator's own machine over the loopback API. Keep the installed proof binding and Wasmtime host available to each tool that runs the registry guests.

[Service API and administration](witchy2/service.md) · [Signing roles, rotation and rollback rules](witchy2/README.md) · [Glamour web UI and security model](witchy2/web/README.md) · [Private candidate staging and release approval](witchy2/approval.md)

The earlier web and publishing examples remain under `coven-web/` and `coven-publish-demo/`. The Witchy 2 server and its guests live in `witchy2/`.
