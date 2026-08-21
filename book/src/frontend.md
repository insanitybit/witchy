# Frontend Apps with Glamour

Glamour is Witchy's Model-View-Update (MVU) frontend framework. It compiles to WebAssembly and runs with zero ambient authority: DOM manipulation, network access, and secrets are capability-governed.

## Minimal Counter

A pure application defines state (`Model`), events (`Msg`), a state transition (`update`), and a view function:

```witchy
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
    glamour.ui(glamour.element("div", [glamour.prop("class", "counter")], [
        glamour.element("button", [glamour.on("click", Dec)], [glamour.text("-")]),
        glamour.element("span", [], [glamour.text("Count: ${m.count}")]),
        glamour.element("button", [glamour.on("click", Inc)], [glamour.text("+")])
    ]))

fn main(console: Console):
    let app = glamour.simple_program(Model(0), update, view)
    let model = update(Model(0), Inc)
    let ui = view(model)
    console.print("State: ${model.count}")
    console.print(glamour.to_html(glamour.ui_node(ui)))
```

```text
State: 1
<div class="counter"><button>-</button><span>Count: 1</span><button>+</button></div>
```

Here is the counter running live in this page with network access denied:

```glamour-app
counter
```

## Program Constructors

Glamour scales from self-contained widgets to full applications:

- **`glamour.simple_program(initial, update, view)`**: Pure state machines without commands or subscriptions. `update` is `fn(model, msg) -> model`.
- **`glamour.command_program(authorize, initial, update, view)`**: Command-driven applications performing capability-gated tasks (HTTP, navigation). `update` is `fn(auth, model, msg) -> (model, Cmd(msg))`.
- **`glamour.program(authorize, initial, start, update, view, subscriptions)`**: Full enterprise applications with global subscriptions (keyboard, timers, storage events).

## Sinkless Views

Views are pure data trees constructed from typed nodes. Text nodes are escaped by construction:

```witchy
import glamour
from glamour import VNode

fn main(console: Console):
    let title = "Dashboard"
    let user_input = "<script>alert('xss')</script>"
    let view: VNode(Int) = glamour.element("section", [glamour.prop("class", "card")], [
        glamour.element("h1", [], [glamour.text(title)]),
        glamour.element("p", [], [glamour.text(user_input)])
    ])
    console.print(glamour.to_html(view))
```

```text
<section class="card"><h1>Dashboard</h1><p>&lt;script&gt;alert('xss')&lt;/script&gt;</p></section>
```

A `<script>` tag inside user data renders as inert text, eliminating XSS by construction.

## Typed Routing and Navigation

Routes are validated patterns compiled into immutable graphs:

```witchy
import glamour

fn main(console: Console):
    match glamour.route("/user/*name"):
        Err(e) -> console.print(glamour.route_error_message(e))
        Ok(route) ->
            match glamour.route_url(route, [("name", "alice")]):
                Err(e) -> console.print(glamour.route_error_message(e))
                Ok(url) -> console.print("URL: " + glamour.safe_url_string(url))
```

```text
URL: /user/alice
```

## Security Invariants

- **Sinkless DOM**: No `innerHTML` or string interpolation into DOM. Nodes are built via `createElement`, `textContent`, and `setAttribute`.
- **Foreign Isolation**: Third-party JavaScript runs inside opaque-origin `<iframe sandbox="allow-scripts">` compartments with `connect-src 'none'`.
- **Host-Custodied Secrets**: Credentials rendered via `SecretInput` stay in host custody (`SecretRef`) and never cross into WebAssembly memory.
- **Derived CSP**: Content Security Policies are derived mathematically from declared capability footprints via `witchy caps --csp`.
- **CSRF Immunity**: State-changing endpoints enforce `Sec-Fetch-Site: same-origin` and require typed route tokens.


