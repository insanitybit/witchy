# coven-web — security model & threat model

coven-web is a web frontend for the coven registry built for **zero tolerance to XSS/CSRF**,
**assuming XSS/CSRF happen anyway**, and **assuming any dependency may be malicious**. This
document records the guarantees, the layers that provide them, and the accepted residual risk.
The full design is in [`PLAN.md`](PLAN.md); this is the security summary.

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
   `nosniff`; `no-referrer`; `no-store`. State-changing requests use a Sec-Fetch CSRF check **and**
   a fresh, single-use **WebAuthn assertion verified server-side** — never a session bearer alone
   (see "every write is a verified assertion" below). There is **no** plain, session-only write
   route: the only state-changing endpoints are `/api/coven/promote-2fa` and `/api/coven/yank-2fa`.
4. **Zero runtime dependencies.** The frontend is a witchy rune we author + a thin host shell;
   there is no third-party runtime code in the trusted surface (`web/package.json` deps `{}`).
   Build tools (esbuild/tsc/oxlint) are vendored + pinned under `web/tools/`. The only third-party
   code that can run in the browser is a charting library, and it runs **only inside a
   compartment** (opaque origin, `connect-src 'none'`), never in the parent.

## What never crosses the trust boundary

Session state, cookies, tokens, and authenticated responses **never enter a compartment, and never
enter the rune**. The host shell makes every authenticated fetch — attaching the bearer session
itself — and the rune only ever receives rendering *data*; the WebAuthn ceremony (register, login,
2FA promote, 2FA yank) runs entirely in the host, so `navigator.credentials` and the token stay at
the edge. A compartment receives only a non-sensitive JSON grant over a `MessageChannel`, and only a
narrow tagged event comes back.

## Trust rule

Publisher-shaped content (package source, publisher-derived record strings) is untrusted, but it is
*data*: glamour cannot turn data into markup, so it all renders **inline** through the host shell's
`textContent`/`createElement` path — no sanitizer, no sandbox. Isolation is reserved for foreign
*code*, which is the only input that obeys neither the capability nor the no-sink discipline.

## Supply-chain isolation: publishing vs. promotion (the CI-compromise boundary)

The registry's release pipeline is deliberately **two operations on two trust levels**, and the
separation is a load-bearing security invariant — not an ergonomic accident:

- **Publish** (`POST /coven/publish`) is a *machine* action. CI authenticates with a short-lived
  OIDC identity token (trusted publishing), and the namespace's org must match the token's
  repository (SEC-023). A successful publish produces a **`Staged`** version and nothing else — a
  staged version is never resolved by `pm` and never served to consumers.
- **Promote** is a *human* action from a **distinct system**, with two supported enforcement
  boundaries. A trusted core Coven verifies a short-lived identity token and requires the
  issuer-signed `amr` claim to attest `mfa` or `webauthn`; it consumes the token's `jti` before
  release. Coven Web instead verifies a fresh, single-use **WebAuthn 2FA assertion** (a passkey
  challenge drawn from the `Rand` CSPRNG, SEC-011) and forwards only to an internal
  anonymous-mode Coven. Both paths enforce **separation of duties**: the promoter identity must
  differ from the publisher. Only promotion flips `Staged → Released`.

**Why this matters:** it bounds the blast radius of a *total CI compromise*. A stolen OIDC token, a
malicious workflow, or a poisoned build dependency can publish a malicious version — but it **cannot
release it**. Making a version consumable requires a human, at a different system, presenting a
passkey. So the worst a compromised pipeline achieves is a staged artifact awaiting human review,
not a package your users will resolve. This is strictly stronger than registries where a publish
token alone makes a version live.

This also reframes the "publish token isn't bound to a specific `name@version`" concern (SEC-022):
because publish only stages and the OIDC token's claims are fixed by the CI provider (no per-artifact
claim is possible — the same constraint npm/PyPI trusted publishing live with), the binding that
matters is the namespace↔repo-org check (SEC-023) plus the human promote gate above, backed by the
signed provenance attestation on each record. The residual — a replayed publish token staging a
*different* version in the same namespace during its short TTL — is contained by the promote gate and
further narrowed by single-use enforcement on the token's `jti`.

**Do not collapse this boundary.** Accepting a token without an issuer-attested MFA method,
exposing Coven Web's anonymous upstream, dropping the WebAuthn check, or relaxing separation of
duties would each let a CI or edge compromise release packages directly.

## Every state-changing web route is a verified WebAuthn assertion (not a session)

coven-web's write surface is deliberately narrow: the **only** state-changing routes are
`POST /api/coven/promote-2fa` and `POST /api/coven/yank-2fa`, and each one **verifies a fresh,
single-use WebAuthn assertion server-side** (`webauthn.verify_assertion` against the registered
credential + a CSPRNG challenge) before it forwards anything to the upstream coven. A bearer
**session is never sufficient authority** to change registry state — it is only a client-side
"you are signed in" marker the SPA uses to reveal the promote/yank controls.

Two further bindings make the assertion unambiguous. **The challenge is bound to the operation.**
`POST /api/webauthn/challenge` records the `op` (`login`/`promote`/`yank`) and, for a write, the
exact `name@version`; the write handler re-checks them, so an assertion minted for one operation
can never be redirected to another. A used challenge is **consumed by content** (cleared to `{}`
with no `b64`), and a missing `b64` is treated as no outstanding challenge, so an assertion is
single-use. **The recorded promoter is the authenticated session subject** — `promoted_by` is read
from the signed bearer, never from the request body, so the separation-of-duties identity coven
enforces cannot be attacker-chosen.

This matters most against an **anonymous upstream coven** (the dev/e2e default), where coven trusts
a client-chosen `promoted_by`/yanker verbatim: there, the web edge's passkey gate *is* the human
check. A plain, session-only `POST /api/coven/promote` (or `/yank`) would bypass it — any session,
mintable by any social login, could flip `Staged → Released` or yank a version with no passkey. Such
routes therefore **do not exist**; there is no session-only write path. The Sec-Fetch CSRF layer
still fronts every write, but it is a second line, not the authorization gate.

That anonymous upstream is an implementation detail of the Coven Web deployment. It must listen
only on a loopback/private boundary reachable by Coven Web and must never be exposed as the public
registry write endpoint. A directly exposed registry must run trusted mode instead; there,
`/coven/promote` ignores the request's `second_factor` marker and derives the signed factor from a
verified OIDC `amr` claim.

**Do not add a session-only write route.** Any new state-changing endpoint must mirror
`h_wa_promote`/`h_wa_yank` and verify a WebAuthn assertion; gating it on `require_session` alone
would reintroduce exactly the bypass removed here.

## Accepted residual risk

A compromised in-sandbox renderer could still side-channel-exfiltrate the *non-sensitive rendering
data it was given* (e.g. timing). This is accepted **by design**: that data is non-sensitive, and
nothing sensitive (session state) ever enters the sandbox. The sandbox's job is to prevent reaching
anything that matters, which the opaque origin + `connect-src 'none'` enforce.

## Browser floor

The Perfect Types model depends on native Trusted Types (`require-trusted-types-for 'script'` +
`trusted-types 'none'`) and modern CSP. There is no safe down-level polyfill (a polyfill would
reintroduce the fallible sink-guarding code the model deletes), so older browsers are
**unsupported**, not degraded.

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
  Untrusted publisher content (package source, metadata) is *data*: it renders inline through the
  same `textContent`/`createElement` path — there is no sanitizer and no sandbox for data, because
  glamour cannot turn data into markup. Only foreign *code* is isolated (into a compartment iframe).
- **Trust shift (accept consciously).** A WASM renderer moves trust from "audit hand-written
  zero-dep TS" to "audit the witchy source + trust the compiler (already the TCB) + a reproducible
  build + a provable empty footprint." The parent's executable artifact grows; that is the trade.

## Known gaps (tracked in [`PLAN.md`](PLAN.md))

- **TLS:** the witchy server is plain HTTP; production terminates TLS at a fronting proxy (needed
  for the `__Host-`/`Secure` cookie). `Strict-Transport-Security` is always sent so the browser
  upgrades every subsequent request.

## Proxy resilience

The reverse proxy dials the upstream coven with a **fallible** connect (`http.try_get` /
`http.try_post`): an unreachable or mid-request-failed upstream yields a clean **502**, never a
crashed server. The rest of the site (static assets, the SPA shell) stays available while coven is
down.
