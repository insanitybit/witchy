# Frontend Apps with Glamour

witchy builds browser UIs the same way it builds everything else: as pure code
with authority carried explicitly. The framework is **Glamour**, a
capability-pure Model-View-Update (MVU) library — and this book you are reading
is itself a Glamour app.

## The shape of a Glamour app

A Glamour app is three pure pieces plus a host that runs the effects:

- a **model** — your application state, an ordinary witchy value;
- a **view** — a pure function from the model to a **`VNode`** tree. A view is
  *data*, never a string: you build `element`/`text`/`prop` nodes, and text is
  escaped by construction, so a `<script>` in your data renders as inert text
  rather than markup (there is no HTML-injection sink);
- an **update** — a pure function from a message and the current model to the
  next model, optionally returning **`Cmd`** values.

A `Cmd` is an **inert description of an effect** — "make this HTTP request",
"navigate here", "arm a timer" — returned as data. The app never performs the
effect itself; it hands the description back, and a capability-holding **host
shell** interprets it. So the app holds no DOM, network, storage, or credential
authority: a witchy UI has the same deny-by-default footprint story as any other
witchy program.

You can see both halves — a pure `VNode` render and a full live MVU counter whose
network authority is denied — running right in the page in
[Appendix: Recipes](appendix-recipes.md).

## The browser boundary is deny-by-omission

A Glamour app compiles to the same witchy WebAssembly as any other program, and
runs on the browser's own WebAssembly engine. The browser host provides only the
pure-compute infrastructure imports; every authority import (`Dir`, `Net`,
`Clock`, `Env`, `Exec`, secrets) is simply **not on offer**. A module that reaches
for one cannot instantiate. That is the same structural guarantee as the native
sandbox, arrived at by omission rather than by a runtime check — see
[the WASM ABI](https://github.com/insanitybit/witchy/blob/master/spec/wasm-abi.md)
and [Capabilities](capabilities.md) for the full model.

This is also why, in the runnable version of this book, a `Console`-only example
has a **Run** button but one reaching for another authority currently does not:
the browser host links those imports as trapping stubs, so the module can't run.
The planned fix (see
[RFC-0091](https://github.com/insanitybit/witchy/blob/master/rfcs/0091-browser-virtual-capabilities.md))
is to back each stub with *what the browser actually has* — the real clock for
`Clock`, the browser's own `fetch` (within its CORS rules) for `Net`, a
default-empty (but page-overridable) environment for `Env` — and an in-memory
scratch tree for `Dir`, the one capability that genuinely needs a backing. Those examples then run in the page (with
ordinary, non-deterministic output, which is fine for a demo), while `Exec` (a
native subprocess) and host secrets have no browser analogue and stay un-runnable
by design.

## UI authority is a capability, too

Some UI effects are sensitive — fetching a URL, navigating, invoking a login or
promote ceremony, reading a password field. Glamour makes each of those a typed,
reviewable **grantable capability**. An app receives one bare root token,
`UiRoot`, and the framework narrows it into per-concern child tokens:

- **`UiFetch`** — construct an HTTP request (scoped to methods / a path prefix);
- **`UiRoute`** — navigate (within a base path);
- **`UiTimer`** — arm a timer;
- **`CredentialPort`** — invoke one named host port (a login, a passkey ceremony,
  a promote);
- **`SecretInput` / `SecretRef`** — render and submit a host-owned secret field
  whose bytes never enter the app.

Each sensitive `Cmd` takes its token as the leading argument, so an unauthorized
effect is *unrepresentable* rather than merely denied at runtime — a component
without a `UiFetch` cannot build an HTTP command at all. The tokens gate
construction; the capability-holding shell still re-checks each effect's policy
before performing it. This is the capability model of
[Capabilities](capabilities.md) applied to UI effects — the same footprint that
[coven](appendix-ecosystem.md) gates on. The full token vocabulary is in the
[capability spec](https://github.com/insanitybit/witchy/blob/master/spec/capabilities.md#framework-effect-authority-capability-safe-ui-glamour).

## Running foreign code safely: compartments and the `Js` capability

Sometimes you need a JavaScript library a witchy app can't replace — a charting
engine, a syntax highlighter. Glamour lets you embed one **without giving up the
security model**, through a *compartment*: an isolated foreign-code bundle the
host mounts in a locked-down, opaque-origin `sandbox="allow-scripts"` iframe with
`connect-src 'none'`, reachable only over a narrow message channel. The foreign
code runs, but it cannot touch the network, the parent origin, or the DOM outside
its frame.

Spawning foreign code is a real authority — the **`Js`** capability, the browser
sibling of `Exec` (RFC-0015). As with every other effect, the app only emits a
*description*; the host shell, which alone holds `Js`, performs the spawn, and
emitting a compartment is what puts `Js` in the app's footprint (so `witchy caps`
surfaces "this rune runs third-party JS"). A component builds the node with
`glamour.compartment`:

```
// `renderer` is a sealed bundle id the host serves under the locked-down origin;
// `grant` is the JSON payload that crosses in (the ONLY thing that does);
// `on_event` names the Reflect variant the compartment's channel may dispatch back.
glamour.compartment("d3-runes-chart", json.stringify(chart_data), "ChartClicked")
```

The shipped example is `projects/glamour/examples/package_page`, whose package
page renders its download chart with an isolated `d3` bundle — d3 runs sandboxed,
and even a compromised d3 is confined to its frame. Coven Web ships that bundle
under `web/dist/compartments/d3-runes-chart/`.

## Where it runs: Coven Web and the docs app

Two shipped apps demonstrate the model end to end:

- **Coven Web** — the web console for the package registry. It pairs a pure-witchy
  server with a thin host shell that holds the browser-side authority a
  pure-compute guest cannot (network, session, credentials), and serves a Glamour
  app same-origin under strict cross-origin isolation.
- **The docs app** — this book, rendered as a Glamour app that fetches each
  chapter and turns its code blocks into editable, runnable cells.

Both are described in [Appendix: The Ecosystem](appendix-ecosystem.md), and their
source lives under
[`projects/`](https://github.com/insanitybit/witchy/tree/master/projects)
(`glamour/`, `coven-web/`, and `docs/`).
