---
rfc: 0008
title: A capability-pure frontend framework (MVU over VNode)
status: implemented
created: 2026-06-22
superseded-by:
tracking: |
  Shipped 2026-06-22 (commits 0783c22 VNode + html tag, de0c68e run-loop + DOM
  shell + String->String export ABI, e56ae24 html end-to-end, 3a7642a effects-as-
  data). The glamour rune (projects/glamour, EMPTY footprint) provides VNode(msg)/
  Attr(msg)/Cmd(msg), the compile-time `html` tag (XSS-immune — text holes become
  text nodes), to_json, and step_with (the MVU engine). The JS host shell
  (web/witchy-runtime/glamour-dom.mjs) diffs VNode->DOM via createElement/
  textContent/setAttribute ONLY, routes events back as Msg values, and interprets
  Cmd — a timer After(ms,msg) the capability-holding shell performs while the rune
  only describes it. counter + autocounter demos with headless DOM tests. Build
  milestone: projects/coven-web/PLAN.md WS-I (the coven-web integration is owned
  separately). REFINEMENTS from implementation: the msg serializer is passed as
  fn(msg)->Json (threading Reflect through nested generics defeated
  monomorphization); Cmd.None renamed NoCmd (Option.None collision).
---

# RFC-0008: A capability-pure frontend framework (MVU over VNode)

> Provisional syntax. Code blocks here are intentionally tagged `text`, **not**
> `witchy`, so the doc-examples test does not try to compile them — names and
> shapes will settle during implementation (WS-I).

## Summary

Ship an Elm/MVU-style frontend framework **as a rune**: a pure
`view(state) -> VNode` / `update(state, msg) -> state` core with a **provably
empty capability footprint**. coven's own analyzer (`compiler.footprint`) proves
the rune touches no `Net`, no `Dir`, no `Clock` — and that proof is the headline.
A thin, capability-holding host shell — TypeScript today — diffs the `VNode` tree
into the real DOM and marshals DOM events back into the program as `Msg` values.
The framework is published **to coven itself**, so the empty footprint is a
public, machine-checked record rather than a claim. coven-web (the registry's web
frontend, [`projects/coven-web/`](../projects/coven-web/)) is the proving ground.

This RFC is the *design*; the build milestone is **WS-I** in
[`projects/coven-web/PLAN.md`](../projects/coven-web/PLAN.md). It is the capstone
of a three-RFC set and **depends on** both
[`RFC-0006`](./0006-compile-time-tagged-literals.md) — *Compile-time tagged
literals* (the `html` ergonomics) — and
[`RFC-0007`](./0007-witchy-wasm-browser-target.md) — *witchy-WASM in the browser:
a pure-compute target* (the execution target). Neither stands alone here: 0006
makes views readable, 0007 makes them runnable and capability-denied, and this
RFC composes the two into a framework.

## Motivation

### Make the capability thesis visceral

witchy's whole pitch is that a program's authority is a statically-computed,
provable footprint. coven-web already makes that the marquee of the *registry*:
it shows you, for each rune, exactly what authority it demands. The natural next
move is to turn the lens on **the UI framework itself**.

A frontend framework is the perfect specimen because the industry's reflex is the
opposite of least-authority: a JavaScript view library can do anything the page
can — `fetch` to any origin, read cookies, touch storage, exfiltrate. "Trust me,
the view layer is pure" is a social promise, not a checked one. witchy can make it
a checked one. The pitch is not "you can write frontends in witchy." The pitch is:

> **JSX, but the compiler proves it can't inject and can't phone home.**

Two independent properties carry that claim:

- **can't inject** — the `VNode` model never forms a `string -> DOM` sink; text
  is always a text node (RFC-0006 makes text holes inert by construction).
- **can't phone home** — the rune's footprint is empty, proven by coven's
  analyzer, and the browser WASM host (RFC-0007) denies every capability import,
  so even a footprint bug has nothing to call.

### Close the last non-witchy tier

coven-web today is a 100%-witchy server (`std/server`) talking to a zero-dep
TypeScript client. The client is TypeScript by necessity — Trusted Types, sandbox
iframes, `MessageChannel`, the HTML Sanitizer are browser APIs (PLAN §1). But the
*application logic* — what to render, how state evolves on an event — has no
reason to live in TypeScript. Moving it into a capability-pure rune closes the
last tier of the stack that is not witchy, and does it in the most load-bearing
possible way: the part a user actually interacts with, proven inert.

## Design

### `VNode` — the view as data

A view is a value, not a side effect. `view(state)` returns a `VNode` tree:

```text
type VNode:
    Element(tag: String, attrs: List(Attr), children: List(VNode))
    Text(String)
    // (later: Keyed(key, VNode) for stable diff identity; Fragment(children))

type Attr:
    Prop(name: String, value: String)   // id, class, href, ...
    On(event: String, msg: Msg)         // a DOM event mapped to a Msg VALUE
```

`Attr` is typed; there is no untyped attribute bag. Crucially `On` carries a
`Msg` **value**, not a closure — the next point is the whole design.

### `Msg` — events are data, not closures

In this framework a DOM event does not invoke a handler closure wired into the
view. It produces a value of a **user-defined `Msg` enum**:

```text
type Msg:
    Increment
    Decrement
    SetText(String)
```

`update` is then an ordinary `match` over those variants:

```text
fn update(state: State, msg: Msg) -> State:
    match msg:
        Increment    -> State(state.count + 1, state.label)
        Decrement    -> State(state.count - 1, state.label)
        SetText(s)   -> State(state.count, s)
```

This is exactly the grain witchy is good at, and it is good at it *because* of
what witchy is. witchy values are **data** — there are no closures captured over
live DOM nodes, no mutable handler graph, none of the higher-order, stateful
machinery that makes other reactive models hard. A view is a pure projection of
state to data; an event is data; `update` is a pure `match` from data to data.
The framework needs no special runtime to make that sound, because the language
already forbids the constructs that would make it unsound. MVU is not a style
imposed on witchy; it is the shape witchy falls into.

### The loop

The framework's entry point is generic over `State` and `Msg` and takes the two
pure functions as first-class values:

```text
fn run(
    initial: State,
    view:    fn(State) -> VNode,
    update:  fn(State, Msg) -> State,
) -> App(State, Msg):
    ...
```

First-class function types are not aspirational here — the stdlib already relies
on them throughout (`std/iter` passes `fn() -> Step(a)` and
`fn(s) -> Option((a, s))` as ordinary values), so `view`/`update` as parameters
are well-trodden ground. Generics over `State`/`Msg` let one framework serve any
app.

The host shell drives the loop; the rune is passive:

1. render `view(state)` to a `VNode` tree;
2. diff it against the previous tree and patch the real DOM;
3. when a DOM event fires on a node carrying `On(event, msg)`, the shell hands
   that `Msg` back across the boundary;
4. compute `update(state, msg) -> state'`, and go to 1.

Steps 2 and 3 — the DOM patch and the event plumbing — are the **only** parts
that touch the platform, and they live entirely in the capability-holding shell.
The rune computes; the shell acts.

### Ergonomic views via RFC-0006's `html`

Constructing `VNode` trees by hand is verbose. [`RFC-0006`](./0006-compile-time-tagged-literals.md)'s
compile-time `html` tagged literal lowers a familiar markup syntax to `VNode`
constructors at compile time:

```text
fn view(state: State) -> VNode:
    html"""
      <div class="counter">
        <button on:click=${Decrement}>-</button>
        <span>${state.count}</span>
        <button on:click=${Increment}>+</button>
        <p>${state.label}</p>
      </div>
    """
```

Two properties follow from RFC-0006 and matter for security:

- **holes are typed by position.** A `${...}` in attribute position must produce
  an `Attr`/`Msg`-shaped value; a `${...}` in element-body position must produce
  text or a `VNode`. The literal is checked at compile time, not stringly at run
  time.
- **text holes become `Text` nodes, never markup.** `${state.label}` is a text
  node whatever the string contains — `<script>` in `label` renders as the four
  literal characters. There is no `string -> markup` path through a hole, so the
  view is **XSS-immune by construction**. This is RFC-0006's guarantee, inherited
  directly.

### Effects as data (the honest hard part)

Real apps need effects: fetch a record, read the clock, store a draft. But the
rune is **capability-denied** — RFC-0007's browser host grants it no `Net`, no
`Clock`, nothing — so the rune *cannot* perform an effect even if it wanted to.
That is a feature, and it forces the design's most interesting move.

`update` does not perform effects; it **describes** them. It returns a `Cmd`
value alongside the new state:

```text
fn update(state: State, msg: Msg) -> (State, Cmd):
    match msg:
        Refresh         -> (state, HttpGet("/api/coven/index", GotIndex))
        GotIndex(body)  -> (parse(state, body), None)
```

A `Cmd` is **data describing a desired effect** — "GET this URL, and turn the
response into *this* `Msg`." The rune emits the description; the
capability-holding host shell **interprets** it — it is the shell that actually
holds `Net`, and the shell decides whether and how to honor the request before
folding the result back in as a `Msg`.

This is the purest expression of witchy's thesis that **authority lives at the
edge**: the witchy core decides *what* should happen; the shell, which alone holds
the capability, decides *whether* it may. A compromised or buggy view can ask for
anything; it can *do* nothing, because asking and doing are separated by the
capability boundary.

Be honest about maturity: **this is the least-built part of the design.** The
first shippable version is render-only — events in, `VNode`s out, no `Cmd` — which
is already enough to migrate coven-web's sandbox highlighter and renderer (PLAN
WS-I). Effects-as-data is where the design risk lives: the `Cmd` vocabulary, how
results are threaded back as `Msg`, and how subscriptions (timers, sockets) fit
all want a spike before they are specified, not pinned down here.

## Security composition

The framework's safety is not one mechanism but the **composition of two
independent containment proofs**, each from a different RFC, neither relying on
the other being correct.

### Where the renderer runs (cite RFC-0007 + PLAN §5)

coven-web's render seam (PLAN §5.2) already runs untrusted-shaped content inside a
double-iframe sandbox. RFC-0007's **parent-vs-sandbox rule** decides where a
witchy-WASM renderer is allowed to live: it runs in the **sandbox first**. The
migration order is deliberate (PLAN WS-I): the syntax **highlighter** moves into a
witchy-WASM module in the sandbox, then the **renderer**, and only much later — if
ever — anything in the trusted parent. The parent stays a tiny, auditable
TypeScript shell; the WASM does the contained work.

### Two independent proofs

1. **Empty footprint** (this RFC + RFC-0006). The rune is proven by coven's
   analyzer to hold no capability. Even granted one by mistake, RFC-0007's host
   denies every capability import, so there is nothing to call.
2. **Iframe sandbox** (PLAN §5.2). The module runs in an opaque-origin,
   `connect-src 'none'` iframe. Even a fired sink reaches nothing: no network, no
   ambient origin, no session.

These are orthogonal. The footprint proof is a property of the *code*; the sandbox
is a property of the *runtime context*. A bug in one is contained by the other.

### `VNode` strengthens Perfect Types

coven-web's parent enforces **Perfect Types** (PLAN §5.1): every legacy
`string -> HTML` injection sink throws, and the only blessed insertion path is the
browser's HTML Sanitizer. The `VNode` model strengthens this rather than competing
with it: a `VNode` tree **never forms a string→DOM sink at all** — the shell walks
it with `createElement` + `textContent`, so there is no HTML string to sanitize in
the first place. The framework's render path is structurally incapable of the very
sink Perfect Types exists to neutralize.

### The crucial caveat: trusted structure, not untrusted content

`html`/`VNode` is for **trusted application structure** authored by the app — it is
XSS-immune by construction *for that role*. It does **not** replace, and must never
be mistaken for, the sandbox + HTML Sanitizer that contain genuinely-untrusted
**publisher** content (a package's own source). Those defenses guard a different,
adversarial input class (PLAN §5, the trust table). They are **complementary, never
substitutes**: use `VNode` to build the app shell; keep publisher-shaped bytes in
the sandbox where they belong. Conflating the two would be a security regression.

## Acceptance

- `compiler.footprint` reports an **empty footprint** for the framework rune — no
  `Net`, `Dir`, or `Clock` — and this is asserted in coven-web's verification.
- coven-web renders through the framework (highlighter, then renderer) with the
  **trusted parent's TypeScript surface no larger than before** — the witchy
  module replaces TS work, it does not add a new trusted surface.
- The framework is **published to coven**, so its empty footprint is a public,
  signed, machine-checked record — the proof, not a claim.

## Alternatives

- **(a) Hand-written `Html` builders, no `html` macro.** Elm's original style:
  `div([class("x")], [text("hi")])`. Works today, no RFC-0006 dependency — a
  viable MVP. The cost is verbosity; RFC-0006 is a pure ergonomic win layered on
  top, not a prerequisite for correctness. A reasonable first cut ships builders
  and adds `html` once 0006 lands.
- **(b) A TypeScript virtual-DOM library** (React, Preact, lit). The status quo
  for the wider world. Rejected: it is not witchy, carries no footprint proof, and
  reintroduces exactly the "trust the view layer" social promise this RFC exists
  to delete.
- **(c) A direct-DOM-manipulation framework** — the rune mutates the DOM itself.
  Rejected for now: it requires a `Dom` capability (RFC-0007 future work), which
  *destroys the empty-footprint property* that is the entire point. MVU's
  data-in/data-out shape is what keeps the footprint empty; direct manipulation
  trades the headline away.

## Drawbacks

- **Verbosity vs JSX.** Even with `html`, the model is more ceremony than ad-hoc
  JSX with inline closures — `Msg` enums and a central `update` are real overhead
  for a trivial widget. Mitigated by RFC-0006; inherent to MVU.
- **Effects-as-data is unbuilt, and it is the hard part.** The `Cmd` vocabulary,
  result threading, and subscriptions are sketched, not specified. This is the
  design's primary risk; a render-only first version sidesteps it but cannot ship
  a real app.
- **Generics-over-`State`/`Msg` ergonomics are unproven.** Passing `view`/`update`
  as `fn` values is well-supported, but a generic `run`/`App(State, Msg)` surface
  threading two type variables through the host boundary has not been built.
  **Recommendation: a spike** before committing the public surface.
- **A WASM renderer is a bigger trusted artifact.** Per RFC-0007's trust shift, a
  compiled WASM module is a larger thing to audit than a few lines of TypeScript.
  The sandbox containment (PLAN §5.2) is what makes that acceptable; outside a
  sandbox it would not be.

## Dependencies

- [`RFC-0006`](./0006-compile-time-tagged-literals.md) — *Compile-time tagged
  literals*. Supplies the `html` literal that lowers to `VNode` and makes text
  holes inert. **Optional for an MVP** (alternative (a) hand-builds `VNode`),
  **required** for the ergonomic surface this RFC describes.
- [`RFC-0007`](./0007-witchy-wasm-browser-target.md) — *witchy-WASM in the
  browser: a pure-compute target*. Supplies the execution target: the host-import
  shim that runs the rune with **all capabilities denied** and the parent-vs-
  sandbox placement rule. **Hard prerequisite** — without it there is no place to
  run the rune, and the empty-footprint guarantee has no teeth.
- [`projects/coven-web/PLAN.md`](../projects/coven-web/PLAN.md) — **WS-I** is the
  build milestone for this design. This RFC is the *design*; WS-I is the *work*:
  the host-import shim (prereq B5), migrating the sandbox highlighter then renderer
  onto the rune, and publishing the framework to coven. PLAN §1 frames the north
  star; PLAN §5 is the security model this RFC composes with.

## Prior art

- **Elm** — the direct ancestor: the Model-View-Update architecture, the `Html`
  builder library this RFC's `VNode` mirrors, and the `Cmd`/`Sub` effects-as-data
  pattern that motivates the `Cmd` section. Elm's "effects are values the runtime
  interprets" is exactly the capability-edge move, arrived at here for a different
  reason (witchy *forbids* the rune from acting; Elm *chooses* to).
- **React / JSX** — the ergonomic comparison and the pitch's foil ("JSX, but the
  compiler proves..."). The `html` literal is the JSX-shaped sugar; the footprint
  proof is what JSX has no analog for.
- **Flux / Redux** — reducers (`(state, action) -> state`) are precisely witchy's
  `update`, and validate the "events are data, state transition is a pure match"
  ergonomics at scale.
- **Capability-secure UI** — the lineage (object-capability systems, confined
  components) in which "the view layer holds no authority" is a design goal rather
  than an accident. This RFC is that idea made statically checkable and published
  as proof.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
