# projects/ — the witchy ecosystem, written in witchy

These are the real, self-hosted applications built *in* witchy (as opposed to
[`examples/`](../examples/), which are small teaching runes). Together they are
the ecosystem story: the language produces packages, a registry distributes
them, a client consumes them, a framework builds UIs, and a web console ties it
together — each dogfooding the language and its capability model.

## The map

```
  witchy (language/compiler/runtime, ../spec, ../crates)
     │  produces
     ▼
   runes ──published──►  coven  ◄──fetch/publish──  pm
                    (registry)      (client CLI)
                        ▲
                        │ serves, same-origin
                   coven-web  ──uses──►  glamour
                   (web console)      (frontend framework)
```

## The components

| Dir | Role | Depends on | Governing design |
|---|---|---|---|
| [`pm/`](pm/) | The package-manager **client** ("cargo for witchy") — resolve, fetch, verify, publish. Pure witchy. | coven (over the HTTP/wire contract) | [rfcs/package-manager.md](../rfcs/package-manager.md) |
| [`coven/`](coven/) | The package **registry** — signed records, source-recomputed capability footprints, block-on-widening, two-phase stage→2FA-promote, TUF. Pure witchy. | witchy `compiler.footprint`, crypto, Dir | [rfcs/package-manager.md](../rfcs/package-manager.md), [spec/local-registry.md](../spec/local-registry.md) |
| [`glamour/`](glamour/README.md) | A capability-pure **MVU frontend framework** — the app computes `VNode` data and emits effects as inert `Cmd` data; a capability-holding host interprets them. | witchy-WASM browser target | [rfcs/0006](../rfcs/0006-compile-time-tagged-literals.md), [0007](../rfcs/0007-witchy-wasm-browser-target.md), [0008](../rfcs/0008-frontend-framework-rune.md), [0039](../rfcs/0039-glamour-capability-safe-effects.md) |
| [`glamour-server/`](glamour-server/README.md) | Optional capability-free **progressive action adapter** — checks Glamour form schemas, request bounds, encoding, and same-origin policy before invoking typed Witchy server callbacks. | glamour, witchy `std/server` | [RFC-0107](../rfcs/0107-glamour-next-generation-web-framework.md) |
| [`coven-web/`](coven-web/) | The **web console** for coven — a pure-witchy server + a thin host shell holding browser authority; serves a glamour app same-origin. | coven (proxied), glamour, witchy server (std/server) | [projects/coven-web/SECURITY.md](coven-web/SECURITY.md), [RFC-0015](../rfcs/0015-secure-web-by-construction.md) |
| [`docs/`](docs/) | The documentation site — the book rendered as a Glamour app and as a typed zero-runtime `Site`. Its `witchy.toml` declares `book/src` as closed `StaticContent`; the static build records every input and emits 56 canonical routes. The runnable public bundle remains the RFC-0041 client path until RFC-0107 resumable islands replace its fresh mount. | glamour, markdown, declared book content | [rfcs/0041](../rfcs/0041-docs-as-a-glamour-app.md), [RFC-0107](../rfcs/0107-glamour-next-generation-web-framework.md) |

## How this differs from `examples/`

`examples/` are single-purpose teaching runes (one concept each, both-backend
e2e-tested). `projects/` are the load-bearing applications the ecosystem itself
runs on — the package manager, the registry, the framework, and the console —
and they are where "witchy all the way down" is actually demonstrated. `pm` and
`coven` are 0-non-witchy-LOC; the frontend components have a principled host
shell in JS at the browser edge (DOM/network/credential APIs a pure-compute
guest cannot and must not hold).

## The two cross-component contracts

These span the Rust ↔ witchy ↔ JS boundaries; treat them as shared source of
truth, not per-consumer copies:

- **The coven HTTP/wire contract** — endpoints, the `/`→`~` package-name
  encoding, and the signed-Record shape. Consumed by pm, coven, coven-web, and
  the coven-web glamour app.
- **The WASM + glamour host ABI** — the guest string/List layout, the `"witchy"`
  import set, and the `VNode`/`Cmd` JSON protocol. Mirrored by the JS runtime
  under [`../web/witchy-runtime/`](../web/witchy-runtime/); see
  [spec/wasm-abi.md](../spec/wasm-abi.md).
