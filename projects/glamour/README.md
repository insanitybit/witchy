# Glamour

Glamour is Witchy's experimental, capability-pure MVU UI substrate. It is not a
React clone and it is not production-ready browser infrastructure yet; it is the
smallest proof that a Witchy application can describe a UI without holding DOM,
network, storage, timer, or credential authority itself.

## Status

- **Status:** experimental prototype.
- **Authority goal:** empty capability footprint for the Glamour core and for
  applications that only compute `VNode` data and `Cmd` descriptions.
- **Trusted boundary:** the Witchy rune computes data; a host shell owns browser
  authority and interprets that data.
- **Primary implementation:** `src/glamour.witchy`.
- **Current examples:** [`examples/`](examples/) includes `counter`, `autocounter`,
  `examples/catalog`, `examples/package_page`, `examples/trust_view`,
  `examples/version_view`, `examples/coven_app`, and `examples/coven_web_app`.

## Model

A Glamour application is ordinary Witchy data flow:

```text
view(state) -> VNode(msg)
update(state, msg) -> (state, Cmd(msg))
```

`VNode(msg)` is inert view data. Events carry typed `msg` values back to
`update`; they are not closures with ambient browser authority. Effects are also
represented as data through `Cmd(msg)`. The host shell decides whether and how to
perform those effects, then dispatches the resulting message back into the rune.

This split is the security property: the application can request navigation,
HTTP, timers, or host ports only by describing them. It does not receive `Net`,
`Clock`, DOM, cookies, WebAuthn, or storage capabilities.

## What is implemented now

- `VNode(msg)` element, text, keyed-node, and compartment data constructors.
- Attribute and event data constructors, including input-value events.
- `Cmd(msg)` descriptions for no-op, timer, batch, HTTP, navigation, and host
  ports.
- JSON serialization for the host-shell protocol.
- HTML serialization helpers used by tests and examples to make escaping visible.
- Example applications that exercise counters, catalogs, trust/version/package
  views, and the Coven Web application shell.

## Not production-ready yet

- The browser host shell and build path still need routine, documented release
  verification before Glamour should be described as stable.
- The empty-footprint claim should remain a CI-enforced invariant for the core
  rune and flagship examples.
- Public docs should continue to label Glamour as experimental until the runtime
  boundary, CSP assumptions, deterministic demo data, and browser tests are all
  documented as a normal release gate.
