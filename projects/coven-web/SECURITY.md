# coven-web — security model & threat model

coven-web is a web frontend for the coven registry built for **zero tolerance to XSS/CSRF**,
**assuming XSS/CSRF happen anyway**, and **assuming any dependency may be malicious**. This
document records the guarantees, the layers that provide them, and the accepted residual risk.
The full design is in `PLAN.md`; this is the security summary.

## Architecture in one line

A 100%-witchy `std/server` (`src/coven_web.witchy`) serves a zero-dependency TypeScript SPA and
reverse-proxies coven's read API **same-origin**, so the browser only ever talks to one origin
(no CORS). Untrusted, publisher-shaped content (package source) renders only inside a
network-firewalled, opaque-origin double-iframe sandbox.

## Defense layers (each independently stops the common case)

1. **Perfect Types in the trusted parent.** The app-shell CSP is
   `require-trusted-types-for 'script'; trusted-types 'none'`. `'none'` is the *strictest* setting,
   not "off": it forbids creating any Trusted Types policy, so every string→HTML sink (`innerHTML`,
   `srcdoc`, `document.write`, …) throws. The parent goes further and **never inserts HTML strings
   at all** — all DOM is built with `createElement`/`textContent` — so there is no sink to reach.
2. **Sandbox containment.** Package source renders in a double iframe: an outer
   `<iframe sandbox="allow-scripts" src="/sandbox-frame">` (own CSP with `connect-src 'none'`;
   opaque origin because `allow-same-origin` is omitted) that bootstraps an inner `srcdoc` iframe
   (a second opaque origin). All traffic is over a private `MessageChannel`. A full XSS inside the
   sandbox cannot read cookies, call the API, navigate to exfiltrate, or reach the parent DOM. This
   is verified: an in-sandbox `fetch()` is blocked by `connect-src 'none'`.
3. **Strict same-origin + cross-origin isolation + CSRF.** Same-origin-only CSP (`connect-src
   'self'`); **strict COOP `same-origin` + COEP `require-corp` + CORP `same-origin`** on *every*
   response (hard invariant — own-process, cross-origin-isolated document, anti-Spectre);
   `X-Frame-Options: DENY` (SAMEORIGIN only for `/sandbox-frame`); deny-all `Permissions-Policy`;
   `nosniff`; `no-referrer`; `no-store`. State-changing requests (v2) use `__Host-` `SameSite=Strict`
   cookies + a Sec-Fetch CSRF check.
4. **Zero runtime dependencies.** The SPA has no third-party runtime code (`web/package.json` deps
   `{}`). Build tools (esbuild/tsc/oxlint) are vendored + pinned under `web/tools/`. The only
   vendored library that runs in the browser (a syntax highlighter, later) executes **only inside
   the sandbox**, never in the parent.

## What never crosses the trust boundary

Session state, cookies, tokens, and authenticated responses **never enter a sandbox**. The trusted
parent makes any authenticated fetch and passes only rendering *data* into the sandbox over the
MessageChannel; only structured events (height, ready) come back.

## Trust rule

If content comes from or is shaped by a publisher (package source, and publisher-derived record
strings), it is treated as untrusted: source renders in the sandbox; short strings render in the
parent via `textContent`.

## Accepted residual risk

A compromised in-sandbox renderer could still side-channel-exfiltrate the *non-sensitive rendering
data it was given* (e.g. timing). This is accepted **by design**: that data is non-sensitive, and
nothing sensitive (session state) ever enters the sandbox. The sandbox's job is to prevent reaching
anything that matters, which the opaque origin + `connect-src 'none'` enforce.

## Browser floor

The Perfect Types model depends on the native HTML Sanitizer / Trusted Types and modern CSP. There
is no safe down-level polyfill (a polyfill would reintroduce the fallible sanitizer-policy code the
model deletes), so older browsers are **unsupported**, not degraded.

## Future: witchy-WASM in the browser (WS-I)

WS-I brings witchy-compiled WASM into the browser (first the sandbox source highlighter/renderer,
later the framework). Full threat model: [RFC-0007](../../rfcs/0007-witchy-wasm-browser-target.md).
The load-bearing rules to preserve:

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
