# Frontend Apps with Glamour

Witchy builds browser UIs with authority carried explicitly. The framework is
**Glamour**, an empty-root-footprint Model-View-Update (MVU) library - and this
book you're reading is itself a Glamour app.

Glamour is engineered so that writing a frontend application produces software
that is **impregnable by construction**: cross-site scripting (XSS), cross-site
request forgery (CSRF), clickjacking, credential exfiltration, and supply-chain
compromises are mathematically impossible under its capability architecture.

## The shape of a Glamour app

A Glamour app separates pure data transformations from the capability-holding
browser shell:

- a **model** - your application state, an ordinary Witchy value;
- a **view** - an ordinary function from the model to a **`VNode`** tree. A view is
  *data*: you build `element`/`text`/`prop` nodes, and text is escaped by
  construction. A `<script>` in your data renders as inert text;
- an **update** - an ordinary function from a message and the current model to the
  next model, optionally returning **`Cmd`** values.

A `Cmd` is an **inert description of an effect** - "make this HTTP request",
"navigate here", "arm a timer" - returned as data. The app never performs the
effect itself; it hands the description back, and a capability-holding **host
shell** interprets it. So the app holds no direct DOM, network, storage, or credential
authority: a Witchy UI has the same deny-by-default footprint story as any other
Witchy program.

## Ergonomic Program Constructors (RFC-0136)

Glamour provides zero-ceremony constructors tailored to the complexity of your
application:

### 1. Pure State Machines (`simple_program`)

For self-contained interactive components and islands that need no side-effects or
subscriptions, `simple_program` eliminates all lifecycle boilerplate:

```text
import glamour
from glamour import Ui, VNode

type Model:
    count: Int

type Msg:
    Inc
    Dec

fn update(m: Model, msg: Msg) -> Model:
    match msg:
        Inc -> Model(m.count + 1)
        Dec -> Model(m.count - 1)

fn view(m: Model) -> Ui(Msg):
    glamour.ui(jsx"<div><button on:click=${Dec}>-</button><span>${m.count}</span><button on:click=${Inc}>+</button></div>")

pub fn app():
    glamour.simple_program(Model(0), update, view)
```

### 2. Command-Driven Applications (`command_program`)

When an application performs capability-gated commands (such as HTTP fetches or
navigation) but does not require global event subscriptions:

```text
import glamour
from glamour import Cmd, Ui, UiRoot, UiFetch

type Auth:
    fetch: UiFetch

type Model:
    loading: Bool
    data: String

type Msg:
    FetchData
    DataReceived(String)

fn authorize(root: UiRoot) -> Auth:
    Auth(glamour.narrow_fetch(root, ["GET"], "/api/v1/"))

fn update(auth: Auth, m: Model, msg: Msg) -> (Model, Cmd(Msg)):
    match msg:
        FetchData ->
            (Model(true, m.data), glamour.http_get("req-1", auth.fetch, "/api/v1/status", DataReceived))
        DataReceived(content) ->
            (Model(false, content), NoCmd)

pub fn app():
    glamour.command_program(authorize, Model(false, ""), update, view)
```

### 3. Full Enterprise Applications (`program`)

For full-scale applications with timers, keyboard listeners, worker pools, and
custom port bindings, the 6-parameter `glamour.program(authorize, initial, start, update, view, subscriptions)`
exposes complete lifecycle control.

## Impregnable Security Architecture

Glamour is designed to drive the cost of exploitation to zero-day / state-actor
levels by eliminating entire bug classes structurally:

### 1. Sinkless DOM Construction

Glamour never calls `innerHTML`, `outerHTML`, or `document.write()`. DOM nodes are
assembled exclusively via `document.createElement()`, `node.textContent = ...`, and
`element.setAttribute()`. All element tag names are checked against a strict HTML5
element allowlist (`SAFE_ELEMENTS`). Event handlers are bound via `addEventListener`
using reflected message IDs, preventing any string evaluation or event-handler
injection (`onload=...`, `onerror=...`).

### 2. Foreign Code Compartments (`Js` Authority)

Third-party libraries (such as D3 charts or code highlighters) cannot run in the
main page context. Glamour mounts them inside opaque-origin `<iframe sandbox="allow-scripts">`
containers with `connect-src 'none'`. They communicate solely over a bi-directional
`MessageChannel` with validated JSON grants, completely unable to touch cookies,
local storage, or the surrounding DOM.

```text
// Mount an isolated D3 chart compartment
glamour.compartment("d3-runes-chart", json.stringify(chart_data), "ChartClicked")
```

### 3. Host-Custodied Secrets (`SecretInput` / `SecretRef`)

Password fields, API keys, and session tokens are never held in the Wasm guest memory.
Glamour uses host-managed `SecretInput` fields that render opaque handles (`SecretRef`).
When forms are submitted, the browser host submits the credential directly through a
sealed port, ensuring guest code (and any compromised dependencies) can never read the
secret bytes.

### 4. Compiler-Automated Derived CSP (RFC-0137)

Content Security Policies are not written by hand; the Witchy compiler derives
mathematically minimal CSP headers directly from your application's capability footprint:

```sh
$ witchy caps --csp app.witchy
default-src 'none'; connect-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; frame-src 'none'; base-uri 'none'; form-action 'self'; require-trusted-types-for 'script'; trusted-types glamour;
```

If an app holds no `Fetch` or `UiFetch` capability, `connect-src 'none'` is emitted,
ensuring the browser hardware physically refuses outbound network traffic.

### 5. Cross-Origin Defense and CSRF Immunity

State-changing endpoints in Glamour servers strictly enforce `Sec-Fetch-Site: same-origin`
and reject cross-origin form posts. Client navigation URLs are typed tokens (`RouteDef` /
`SafeUrl`), preventing open redirects and parameter tampering.

## Templates are Witchy Tagged Literals

Glamour does not add JavaScript JSX to the language. `html"..."` and `jsx"..."`
are ordinary compile-time tags, in the same family as `sql"..."`:
Glamour parses the static text and the compiler preserves each Witchy `${...}`
hole as typed syntax. Both spellings produce the same checked template plan.

```text
let view: VNode(Msg) = jsx"""
    <section class=${styles.card}>
        <h1>${model.title}</h1>
        <button on:click=${Save}>Save</button>
    </section>
"""
```

Text holes stay text nodes. URL, boolean, property, class, ARIA, and event
positions use distinct typed sinks, so a plain `String` can't accidentally
become a navigation URL. `css"..."` similarly compiles a static, scoped sheet
with deterministic class handles.

## UI Authority is a Capability

Some UI effects are sensitive - fetching a URL, navigating, invoking a login or
promote ceremony, reading a password field. Glamour makes each of those a typed,
reviewable **grantable capability**. An app receives one bare root token,
`UiRoot`, and the framework narrows it into per-concern child tokens:

- **`UiFetch`** - construct an HTTP request (scoped to methods / a path prefix);
- **`UiRoute`** - navigate (within a base path);
- **`UiTimer`** - arm a timer;
- **`CredentialPort`** - invoke one named host port (a login, a passkey ceremony,
  a promote);
- **`SecretInput` / `SecretRef`** - render and submit a host-owned secret field
  whose bytes never enter the app.

Each sensitive `Cmd` takes its token as the leading argument, so an unauthorized
effect is *unrepresentable* rather than merely denied at runtime. The tokens gate
construction; the capability-holding shell still re-checks each effect's policy
before performing it. The full token vocabulary is in the
[capability spec](https://github.com/insanitybit/witchy/blob/master/spec/capabilities.md#framework-effect-authority-capability-safe-ui-glamour).

## Where it runs: Coven Web and the docs app

Two shipped apps demonstrate the model end to end:

- **Coven Web** - the web console for the package registry. It pairs a server
  implemented in Witchy with a thin host shell that holds the browser-side authority a
  zero-root-authority guest can't (network, session, credentials), and serves a Glamour
  app same-origin under strict cross-origin isolation.
- **The docs app** - this book, rendered as a Glamour app that fetches each
  chapter and turns its code blocks into editable, runnable cells.

Both are described in [Appendix: The Ecosystem](appendix-ecosystem.md), and their
source lives under
[`projects/`](https://github.com/insanitybit/witchy/tree/master/projects)
(`glamour/`, `coven-web/`, and `docs/`).

