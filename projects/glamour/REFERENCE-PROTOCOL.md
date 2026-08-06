# Glamour JSON reference protocol

Status: semantic reference for RFC-0107 Phase 0.

This document freezes the observable behavior of the current Glamour host. The
optimized protocol may use a different representation, but it must produce the
same application state, DOM, commands, security decisions, and lifecycle unless
a later RFC records an intentional change.

## Static template plans

A checked `html` or `jsx` literal wraps its ordinary reference VNode:

```json
{
  "plan": {
    "version": 1,
    "id": "glamour-tp1-<sha256>",
    "origin": "module:line",
    "slots": [
      {"index": 0, "kind": "text", "name": ""},
      {"index": 1, "kind": "url", "name": "href"}
    ]
  },
  "node": {"el": "a", "attrs": [], "kids": []}
}
```

The ID hashes normalized static construction and slot metadata; source location
is diagnostic data and does not affect identity. The Phase 2 host validates and
interns plans before DOM writes, then executes the enclosed reference VNode.
Phase 3 replaces that enclosed tree with changed-slot patches.

Preferred attribute records are `attr`, `bool`, `property`, `url`, `class`, and
`aria`. The compatibility `prop` record remains accepted. The host rechecks
names, URL schemes, property allowlists, and sink category at the authority
boundary.

## Transport

The application exports a synchronous `String -> String` function named
`__export_export_step` in Wasm. The host retains the latest model as a JavaScript
value and calls the export with UTF-8 JSON:

```json
{"model":0}
{"model":0,"msg":{"$variant":"Increment","$values":[]}}
```

The application returns exactly one result object:

```json
{
  "model": 1,
  "vnode": {"text": "1"},
  "cmd": {"cmd": "none"}
}
```

`model` is opaque to the host except for JSON serialization. `msg` is the value
embedded by an event, command, route callback, host port, compartment, or secret
status event. An application boundary error is `{"error":"message"}`; the host
throws before changing its retained model or DOM.

The current transport sends the full model into Wasm and receives the full
model and VNode after every event. RFC-0107 treats that cost as baseline
behavior, not as the production 1.0 architecture.

### Program extension

RFC-0107 Program mode starts with:

```json
{"start":{"route":"/","bootstrap":""}}
```

The response adds `subs`. Subsequent calls retain the ordinary `{model,msg}`
shape. Compatibility applications continue to use `{model}` for their initial
call; the host selects Program mode only when `mount` receives `opts.start`.

## VNodes

| Kind | JSON shape | Host behavior |
| --- | --- | --- |
| text | `{"text":"value"}` | Create or update one text node. |
| element | `{"el":"div","attrs":[],"kids":[]}` | Create an allowlisted element and reconcile it structurally. |
| keyed | `{"key":"stable","node":vnode}` | Match sibling identity by key and move the existing DOM node. |
| compartment | `{"compartment":"renderer","grant":"json","on":"Tag"}` | Mount a sandboxed iframe and a narrow message channel. |
| secret | `{"secret":{"form":"f","field":"p"},"on_ready":"Ready"}` | Mount a host-custodied password input; its value never enters model or messages. |
| slot | `{"slot":"kind","data":"payload"}` | Let a registered host renderer own a non-diffed subtree. |

Sibling keys must be unique. Duplicate keys fail before reconciliation. A
malformed or unknown node fails closed.

Only the element allowlist in `glamour-dom.mjs` reaches `createElement`.
Disallowed names become inert `span` elements. The host never uses
`innerHTML`, `outerHTML`, `insertAdjacentHTML`, or an equivalent markup sink.

### Attributes and events

```json
["prop", "class", "card"]
["on", "click", {"$variant":"Selected","$values":[7]}]
["oninput", "input", "QueryChanged"]
["decode", "input", "profile.name", "value", false, false]
```

Property names and event names must match the pinned ASCII grammars. Invalid
names become inert diagnostic data attributes. String attributes beginning
with `on` and `srcdoc` are dropped. URL-bearing attributes pass through the
host URL policy; unknown and executable schemes become `#`.

`on` dispatches its embedded message unchanged. `oninput` reads
`event.target.value` and dispatches the reflected one-argument variant named by
the third field. Reconciliation replaces old listeners instead of accumulating
them.

`decode` carries event name, stable decoder ID, allowlisted field kind,
`preventDefault`, and `stopPropagation`. The host returns only:

```json
{
  "$glamour_event": "profile.name",
  "value": "Ada",
  "checked": false,
  "key": ""
}
```

Witchy recreates the old `Ui`, finds that ID, and runs its typed decoder. An
unknown or filtered decoder is inert. A browser `Event`, target, DOM handle,
file, clipboard, or arbitrary property never crosses the boundary.

## Commands

| Command | Required fields | Result |
| --- | --- | --- |
| none | `{"cmd":"none"}` | No work. |
| after | `ms`, `min_ms`, `msg` | Clamp to `max(ms,min_ms)`, then dispatch `msg`. |
| stable after | `id`, `ms`, `min_ms`, `msg` | Replace the same ID; stale generations are inert. |
| cancel | `id` | Cancel the current stable command generation. |
| batch | `cmds` | Interpret children in list order. |
| typed http | `id`, `method`, `url`, `body`, `reply_a`, `reply_b`, `methods`, `prefix`, `scope` | Recheck policy, fetch at the host, fill the typed `HttpResult` slot, and dispatch the completed message. |
| typed nav | `id`, `path`, `reply_a`, `reply_b`, `base`, `rights` | Recheck the base, push history, fill `NavigationResult`, and dispatch the completed message. |
| typed port | `id`, `port`, `arg`, `reply_a`, `reply_b` | Invoke an allowed port, fill `PortResult`, and dispatch the completed message. |
| typed submit_secret | `id`, `slot`, `port`, `reply_a`, `reply_b` | Submit a rendered host-held secret, fill `PortResult`, and dispatch only that result. |
| compatibility http/nav/port/secret | prior `tag` shapes | Preserve the RFC-0008 host protocol through explicitly named `_compat` constructors while applications migrate. |

Unknown command kinds throw. Policy violations fail closed before the browser
operation. Typed transport failures become `HttpFailure` or `PortFailure`.
Compatibility HTTP failures use status `0`; compatibility port failures use an
`"error:"` result string. The application never receives cookies, bearer tokens,
WebAuthn credentials, password bytes, DOM handles, or browser globals.

Typed callbacks are ordinary Witchy function values and remain inside Wasm.
For the JSON reference transport, Glamour serializes the callback applied to
two protocol probe values. The host requires those messages to differ at
exactly one matching probe slot, then substitutes the real typed result there.
This preserves outer wrappers introduced by `Cmd.map` and rejects callbacks
that erase, duplicate, or branch on the result. Phase 2 replaces this verbose
reference encoding with generated binary completion descriptors.

## Subscriptions

Program responses include one subscription tree:

```json
{"sub":"none"}
{"sub":"every","id":"clock","ms":1000,"min_ms":1000,"msg":message}
{"sub":"batch","subs":[subscription]}
```

The host flattens the tree and requires unique non-empty IDs. An unchanged
fingerprint keeps its host handle. A changed subscription is cancelled before
replacement; a removed subscription is cancelled immediately. Every callback
checks its generation, so a queued callback from a replaced or unmounted source
is inert.

## Lifecycle

Mount performs one start or compatibility model step and creates one root node. An optional route
binding then dispatches the current path. Each dispatch computes a complete next
result, patches the DOM, commits the model and VNode, and interprets the command.

Unmount is idempotent. It removes the root, clears tracked timers, unsubscribes
from routing, cancels every stable command and subscription, erases secret
custody, and makes late timer, subscription, fetch, and port callbacks no-ops.

The host resets the Witchy string-export heap after every call. Linear memory
therefore remains bounded under repeated dispatch.

## Reference evidence

`tests/glamour/dom.rs` maps protocol behavior to dependency-free Node drivers.
The counter, controlled input/catalog, keyed list, router, docs/book,
compartment, secret custody, slot, timer, HTTP, host port, XSS, lifecycle, and
heap-reset cases are executable reference tests.

Run the focused reference suite with:

```sh
cargo nextest run --test glamour -E 'test(/^dom::/)'
```

Run the Phase 0 before-measurement with:

```sh
node web/witchy-runtime/glamour-baseline.mjs target/debug/witchy
```
