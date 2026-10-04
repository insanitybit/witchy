# projects/ — the witchy ecosystem, written in witchy

These are applications built *in* witchy that still live in this repository
(as opposed to [`examples/`](../examples/), which are small teaching runes).
The package manager, grimoire, and the registry, coven, are their own projects
beside this one: grimoire drives the compiler the way cargo drives rustc.

## The components

| Dir | Role | Depends on | Governing design |
|---|---|---|---|
| [`glamour/`](glamour/README.md) | An empty-root-footprint **MVU frontend framework** — the app computes `VNode` data and emits effects as inert `Cmd` data; a capability-holding host interprets them. | witchy-WASM browser target | [rfcs/0006](../rfcs/0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries), [0007](../rfcs/retired-rfcs.md), [0008](../rfcs/retired-rfcs.md), [0039](../rfcs/retired-rfcs.md) |
| [`glamour-server/`](glamour-server/README.md) | Optional capability-free **progressive action adapter** — checks Glamour form schemas, request bounds, encoding, and same-origin policy before invoking typed Witchy server callbacks. | glamour, witchy `std/server` | [RFC-0107](../rfcs/retired-rfcs.md) |
| [`docs/`](docs/) | The documentation site — the book rendered as a Glamour app and as a typed zero-runtime `Site`. Its `witchy.toml` declares `book/src` as closed `StaticContent`; the static build records every input and emits 56 canonical routes. The runnable public bundle remains the RFC-0041 client path until RFC-0107 resumable islands replace its fresh mount. | glamour, markdown, declared book content | [rfcs/0041](../rfcs/retired-rfcs.md), [RFC-0107](../rfcs/retired-rfcs.md) |

## How this differs from `examples/`

`examples/` are single-purpose teaching runes (one concept each). `projects/`
are larger applications: the Glamour framework and the documentation site
built with it. Their frontend components have a principled host shell in JS at
the browser edge (DOM/network/credential APIs a zero-root-authority guest
cannot and must not hold).

## The cross-component contract

- **The WASM + glamour host ABI** — the guest string/List layout, the `"witchy"`
  import set, and the `VNode`/`Cmd` JSON protocol. Mirrored by the JS runtime
  under [`../web/witchy-runtime/`](../web/witchy-runtime/); see
  [spec/wasm-abi.md](../spec/wasm-abi.md).
