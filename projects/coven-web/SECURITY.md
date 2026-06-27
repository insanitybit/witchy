# coven-web — security model & threat model

coven-web is a web frontend for the coven registry built for **zero tolerance to XSS/CSRF**,
**assuming XSS/CSRF happen anyway**, and **assuming any dependency may be malicious**. This
document records the guarantees, the layers that provide them, and the accepted residual risk.
The full design is in `PLAN.md`; this is the security summary.

## Architecture in one line

A 100%-witchy `std/server` (`src/coven_web.witchy`) serves a single **capability-pure glamour
WASM rune** as the entire frontend (`projects/glamour/examples/coven_web_app`, empty footprint —
compiled and base64-inlined into `app.js`; the only hand-written JS is a thin host shell holding
all authority) and reverse-proxies coven's read API **same-origin**, so the browser only ever
talks to one origin (no CORS). The rune builds the DOM through glamour's host shell —
`createElement`/`textContent` only, never an HTML string — so it is **XSS-immune by
construction**; untrusted publisher content (package source, metadata) is *data* and renders
inline, safe by construction. Foreign *code* (a third-party charting library) is the only thing
isolated, into an opaque-origin compartment iframe.

## Defense layers (each independently stops the common case)

1. **Perfect Types in the trusted parent.** The app-shell CSP is
   `script-src 'self' 'wasm-unsafe-eval'; … require-trusted-types-for 'script'; trusted-types 'none'`.
   `trusted-types 'none'` is the *strictest* setting, not "off": it forbids creating any Trusted
   Types policy, so every string→HTML sink (`innerHTML`, `srcdoc`, `document.write`, …) throws. The
   frontend goes further and **never inserts HTML strings at all** — the glamour host shell builds
   all DOM with `createElement`/`textContent`/`setAttribute` (and the attribute layer drops `on*`
   props and scheme-checks URL attributes), so there is no sink to reach. `'wasm-unsafe-eval'` is
   the *only* relaxation: it lets the parent compile the trusted app module (our own inlined rune),
   permits WebAssembly compilation but **not** JS `eval()`/`Function()` (strictly narrower than
   `'unsafe-eval'`), and the only module ever compiled is that rune.
2. **Foreign-code compartments (RFC-0015).** The frontend renders untrusted *data* inline (it can't
   produce a sink), so a compartment is reserved for the genuinely uncontainable case: third-party
   *code*. A compartment is loaded into `<iframe sandbox="allow-scripts">` (opaque origin — no
   cookies, no parent DOM, no storage) served with its own `connect-src 'none'` CSP (no network
   egress); only a non-sensitive JSON grant crosses in over a private `MessageChannel`, and only a
   narrow tagged event comes back. A fully-compromised renderer (a swapped/XSS'd library) cannot
   read cookies, call the API, exfiltrate, or reach the parent DOM. The same opaque-origin
   double-iframe machinery (`/sandbox-frame`) also still backs the optional in-sandbox source
   highlighter, kept served for that use.
3. **Strict same-origin + cross-origin isolation + CSRF.** Same-origin-only CSP (`connect-src
   'self'`); **strict COOP `same-origin` + COEP `require-corp` + CORP `same-origin`** on *every*
   response (hard invariant — own-process, cross-origin-isolated document, anti-Spectre);
   `X-Frame-Options: DENY` (SAMEORIGIN only for `/sandbox-frame`); deny-all `Permissions-Policy`;
   `nosniff`; `no-referrer`; `no-store`. State-changing requests (v2) use `__Host-` `SameSite=Strict`
   cookies + a Sec-Fetch CSRF check.
4. **Zero runtime dependencies.** The frontend is a witchy rune we author + a thin host shell;
   there is no third-party runtime code in the trusted surface (`web/package.json` deps `{}`).
   Build tools (esbuild/tsc/oxlint) are vendored + pinned under `web/tools/`. The only third-party
   code that can run in the browser is a charting library, and it runs **only inside a
   compartment** (opaque origin, `connect-src 'none'`), never in the parent.

## What never crosses the trust boundary

Session state, cookies, tokens, and authenticated responses **never enter a compartment, and never
enter the rune**. The host shell makes every authenticated fetch — attaching the bearer session
itself — and the rune only ever receives rendering *data*; the WebAuthn ceremony (register, login,
2FA promote) runs entirely in the host, so `navigator.credentials` and the token stay at the edge.
A compartment receives only a non-sensitive JSON grant over a `MessageChannel`, and only a narrow
tagged event comes back.

## Trust rule

Publisher-shaped content (package source, publisher-derived record strings) is untrusted, but it is
*data*: glamour cannot turn data into markup, so it all renders **inline** through the host shell's
`textContent`/`createElement` path — no sanitizer, no sandbox. Isolation is reserved for foreign
*code*, which is the only input that obeys neither the capability nor the no-sink discipline.

## Accepted residual risk

A compromised in-sandbox renderer could still side-channel-exfiltrate the *non-sensitive rendering
data it was given* (e.g. timing). This is accepted **by design**: that data is non-sensitive, and
nothing sensitive (session state) ever enters the sandbox. The sandbox's job is to prevent reaching
anything that matters, which the opaque origin + `connect-src 'none'` enforce.

## Browser floor

The Perfect Types model depends on the native HTML Sanitizer / Trusted Types and modern CSP. There
is no safe down-level polyfill (a polyfill would reintroduce the fallible sanitizer-policy code the
model deletes), so older browsers are **unsupported**, not degraded.

## witchy-WASM in the browser (shipped)

The frontend *is* witchy-compiled WASM: the whole app is a glamour rune (RFC-0008/0015), and the
optional source highlighter is a second one. Full threat model:
[RFC-0007](../../rfcs/0007-witchy-wasm-browser-target.md). The load-bearing rules, all upheld:

- **Pure-compute by construction.** The browser host shim implements only the non-capability
  `"witchy"` imports and **denies every capability import** (Net/Dir/Clock/…); a module that needs
  authority simply fails to instantiate. A footprint-empty rune is the *static* form of what the
  iframe sandbox enforces dynamically — two independent containment proofs, not one.
- **Parent vs. sandbox.** WASM runs in the null-origin sandbox (or a worker) by **default**.
  Placing it in the trusted parent requires `script-src 'wasm-unsafe-eval'` — a CSP relaxation —
  and is permitted only as a deliberate, documented decision, never by drift.
- **VNode, not HTML.** The renderer builds DOM via `createElement`/`textContent`/`setAttribute` and
  never forms a string→DOM sink, so it *strengthens* Perfect Types rather than competing with it.
  `html`/`VNode` is for trusted app structure only — untrusted publisher content still goes through
  the sandbox + Sanitizer.
- **Trust shift (accept consciously).** A WASM renderer moves trust from "audit hand-written
  zero-dep TS" to "audit the witchy source + trust the compiler (already the TCB) + a reproducible
  build + a provable empty footprint." The parent's executable artifact grows; that is the trade.

## Known gaps (tracked in PLAN.md)

- **B6:** if the upstream coven is unreachable, the proxy currently crashes the server (`connect`
  raises a fatal, unrecoverable error). Fix = a fallible `connect` returning 502. Shared-tree.
- **TLS:** the witchy server is plain HTTP; production terminates TLS at a fronting proxy (needed
  for the `__Host-`/`Secure` cookie).
