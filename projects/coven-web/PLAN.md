# Coven Registry Secure Web Frontend — Unified Implementation Plan

A single, self-contained implementation plan spanning **both** halves of the initiative:
1. the **witchy/stdlib backend changes** the frontend requires (incl. a crypto-backend
   migration), and
2. the **coven-web** secure web frontend itself.

Status: **WS-A through WS-I shipped and browser-verified.** The frontend is now ONE capability-pure
glamour WASM rune (`projects/glamour/examples/coven_web_app`, empty footprint), compiled and
base64-inlined into `app.js`; the hand-written JS is just a thin host shell (the bootstrap + the
session/WebAuthn/yank ports). All views are glamour — catalog with capability-aware search +
color-coded footprint chips, the signed version record, generated API docs, registry trust, and
inline package **source** (rendered as data, no sandbox needed) — and register/login/promote/yank
run through host ports. The TypeScript views/app-logic are deleted. The whole shell was driven in a
real browser against a live registry (WASM-in-parent under the hardened CSP, client routing +
history fallback, full WebAuthn register→login→2FA-promote via a virtual authenticator). Design +
status: [RFC-0015](../../rfcs/0015-secure-web-by-construction.md). The workstreams below are the
original ordered plan, kept for the backend/stdlib history; everything needed to implement is inline.

> **Design RFCs (2026-06-22).** WS-I is now specified across three proposed RFCs:
> [RFC-0008 — A capability-pure frontend framework](../../rfcs/0008-frontend-framework-rune.md)
> (the MVU-over-`VNode` framework), which depends on
> [RFC-0006 — Compile-time tagged literals](../../rfcs/0006-compile-time-tagged-literals.md)
> (the typed, XSS-immune `html` ergonomics) and
> [RFC-0007 — witchy-WASM in the browser](../../rfcs/0007-witchy-wasm-browser-target.md)
> (the pure-compute browser target; **B5** below is its host-import shim). Build WS-I from those.

---

## 1. Objective & theses

Build a web frontend for **coven** (witchy's package registry) that browses runes, versions,
**capability footprints**, provenance, and **source**, and (later) drives the **promote ("2FA to
publish")** flow — with an uncompromising security posture: **zero tolerance for XSS/CSRF, built
assuming XSS/CSRF happen anyway, and assuming every dependency may be malicious.**

Two theses must land together:
- **witchy's capability model** — a rune's authority is a statically-computed, provable footprint;
  the UI makes this the headline.
- **containment** — untrusted, publisher-shaped content (package source) renders only inside
  null-origin, network-firewalled iframe sandboxes; the trusted parent is a tiny, auditable shell.

**Dogfood stance:** the server is **100% witchy** (`std/server`); the browser client is **zero-dep
TS** (unavoidable — Trusted Types / Perfect Types, sandboxed iframes, `MessageChannel`, the HTML
Sanitizer are browser APIs). witchy-WASM enters later only in *contained* roles (sandbox renderer,
client-side verifier), never as the trusted parent's DOM driver.

**North star (later):** a witchy *frontend framework* shipped **as a rune** — a pure
`view(state) -> VNode` / `update(state, msg) -> state` core with a **provably empty capability
footprint** (coven's own analyzer proves it touches no Net/Dir/Clock), published *to coven itself*
as the proof. coven-web is its proving ground.

## 2. Scope & non-goals

In scope: the witchy/stdlib backend changes (WS-A, WS-B), the coven-web witchy server (WS-C), the
zero-dep TS client + Perfect Types (WS-D), read-only browse (WS-E), the sandboxed source viewer
(WS-F), the trust panel (WS-G), the promote/2FA write flow (WS-H), and the framework-rune north
star (WS-I).

Out of scope / non-goals:
- **No changes to coven's wire protocol or `src/pm/` / `projects/coven/`.** Coven's HTTP API is a
  **frozen contract** (§4). Another agent owns that tree; we stay isolated.
- **No CORS on coven.** The frontend is strictly same-origin (the server proxies coven).
- **Publishing from the browser** is out — publishing stays a CI/trusted-publisher flow. The UI's
  only write surface is **promote / yank**.
- No FIPS 140-3 *validation* in this initiative (we use FIPS-*approved algorithms*; formal CMVP
  validation is a separate future effort — see WS-A).

## 3. Architecture

Two **separate** processes, same-origin to the browser:

```
 browser ──(same-origin, https in prod)──▶  coven-web  (witchy, std/server)
                                              │  serves: app shell, /sandbox-frame,
                                              │          static assets, security headers
                                              └──(std/http, Net[Connect])──▶ coven  (unchanged)
                                                     reverse-proxy /api/coven/* → /coven/*
```

- **coven-web** = a NEW witchy program (`projects/coven-web/src/coven_web.witchy`) on `std/server`.
  It serves the SPA + the full security-header stack and **reverse-proxies** coven's JSON API
  same-origin, so the browser's `connect-src 'self'` CSP is satisfied without CORS.
- **coven** = unchanged; the frozen HTTP contract in §4.
- Future consolidation into one process (router `.nest("/coven", …)`) is possible once coven
  settles — deferred for isolation.

## 4. Frozen interface — coven HTTP contract (verified against current code)

`name` query encodes `/` → `~` and the server does **no** URL-decoding (`src/pm/wire.rs:92`), so
clients send `name=ns~name`. Default coven address `127.0.0.1:8787`; remote selected via `COVEN_URL`.

**Reads (anonymous GET):**
- `GET /coven/index` → `{ "names": ["ns/name", …] }`
- `GET /coven/versions?name=ns~name` → `{ "records": [Record, …] }`
- `GET /coven/record?name=ns~name&version=1.0.0` → `Record` (404 `{ "error": … }`)
- `GET /coven/source?name=ns~name&version=1.0.0` → `{ "files": [[path, text], …] }`
- `GET /coven/rootpub` → root pubkey hex (Rust: text/plain; witchy: JSON-wrapped — proxy normalizes)
- `GET /coven/snapshot` → `{ "signed": { "version", "created", "targets": {"n@v":"sha256:…"} }, "sig" }`
- `GET /coven/timestamp` → `{ "signed": { "snapshot_version", "snapshot_hash", "expires" }, "sig" }`

**Writes (POST; identity token required when issuers registered):**
- `POST /coven/publish` (CI/trusted-publisher; **not** a web action) → staged `Record`
- `POST /coven/promote` `{ name, version, second_factor, id_token }` →
  `{ record, separation_of_duties, delta_runtime, delta_build }` (400/401/403/404 on failure)
- `POST /coven/yank` `{ name, version, id_token }` → `{ "ok": true }`

**Record:** `name, version, state(staged|released|yanked), hash, runtime_footprint[],
build_footprint[], determinism, uploaded_by, promoted_by?, second_factor?, provenance?,
released_at, sig`.

Note coven already implements everything the frontend needs end-to-end (two-phase
publish→promote with second-factor + separation-of-duties, TUF snapshot/timestamp, trusted
publishing). **No coven-side backend work is required by this plan.**

## 5. Security model (self-contained spec)

The trust rule: **if content comes from or is shaped by a publisher, it renders in a sandbox.**

| coven data | trust | renders in |
| --- | --- | --- |
| `/coven/source` (witchy source) | publisher-controlled, arbitrary | **inner sandbox** (highlighted) |
| Record strings (`uploaded_by`, `promoted_by`, `provenance`, `second_factor`) | claims-derived | parent, via `SafeHtml`/`textContent` |
| `runtime_footprint` / `build_footprint` | validated kinds | parent, escaped (chips) |
| index / version names | validated `[a-z0-9_.-]` | parent, escaped (defense-in-depth) |
| TUF snapshot/timestamp, rootpub | signed | parent; signature/freshness shown |

Four independent defense layers:
1. **Perfect Types** in the trusted parent (§5.1) — string→HTML DOM-XSS sinks are categorically inert.
2. **Sandbox containment** (§5.2) — publisher-shaped content renders in an opaque-origin,
   network-firewalled iframe; a fired sink still reaches nothing sensitive.
3. **Strict same-origin CSP + Sec-Fetch CSRF + `SameSite=Strict` cookies** (§5.3).
4. **Zero runtime dependencies** — no third-party code in the parent; vendored libs run only in
   the sandbox.

### 5.1 Perfect Types (the parent's anti-DOM-XSS model)

App-shell CSP sets `require-trusted-types-for 'script'; trusted-types 'none'`. This is **Perfect
Types** (concept: Jun Kokatsu; paired with `setHTML()` by Frederik Braun):
- `require-trusted-types-for 'script'` makes every legacy string→HTML *injection sink*
  (`innerHTML`/`outerHTML` setters, `document.write`, `insertAdjacentHTML`, `<iframe>.srcdoc`,
  `DOMParser`-into-document, …) reject plain strings — they demand a non-spoofable `TrustedHTML`.
- `trusted-types 'none'` forbids creating **any** TT policy. **`'none'` is the *strictest*
  setting, not "off"** — it removes the only mechanism that could mint `TrustedHTML`.

Net effect: those sinks **throw at runtime, always**. The only string→DOM path left is the
browser's built-in **HTML Sanitizer API**, safe by construction (the engine sanitizes; we author
no sanitizer): `Element.setHTML(str, { sanitizer })`, or `Document.parseHTML(str)` then
`querySelector` + `cloneNode`. Why "perfect": ordinary Trusted Types allows a named policy whose
`createHTML` is app-authored sanitizer code — fallible, must be reviewed/maintained, bugs
reintroduce XSS. Perfect Types deletes that code; safety moves to the browser engine.

In our code: create **no** TT policy; a `SafeHtml` type (minted only via an auto-escaping tagged
template or an explicit `rawHtml` trust boundary) is the only thing fed to `setHTML`; any stray
`innerHTML =` throws (fail-closed). Everything else uses `createElement` + `textContent`.

**Browser floor:** `setHTML()`/Sanitizer is new (e.g. Firefox 148, 2026). Target modern evergreen
browsers; state the floor explicitly. No safe down-level polyfill (a polyfill reintroduces the
policy code Perfect Types removes) → older browsers are unsupported, not degraded.

### 5.2 Sandbox containment (double-iframe)

- Parent creates `<iframe sandbox="allow-scripts" src="/sandbox-frame">`. Loading from a **real URL**
  (not `srcdoc`/`blob:`) is deliberate: it gets its **own** CSP from HTTP headers
  (`connect-src 'none'`), independent of the parent. Omitting `allow-same-origin` forces an
  **opaque origin**.
- `/sandbox-frame` bootstrap (served by coven-web, hardcoded, no user data): guards
  `window.origin === "null"`, accepts one `{type:"init", html}` from `e.source === window.parent`,
  creates an inner `srcdoc` iframe (also `allow-scripts`, a second opaque origin), relays a
  `MessagePort` to it.
- All real traffic flows over the private `MessageChannel`, never `window.postMessage`. Session
  state never crosses; only rendering data goes in, only structured events come out.

**Stable render RPC** (so a witchy-WASM highlighter can replace highlight.js later, WS-I):
- parent → sandbox: `{ type: "render", kind: "witchy-source", text: <string> }`
- sandbox: `render(kind, text) -> htmlString` (v1 highlight.js; later witchy-WASM, zero caps)
- sandbox → parent: `{ type: "height", px }`, `{ type: "ready" }`

### 5.3 Headers, CSP, CSRF

**CSP per route class (exact strings):**
- App shell (HTML) — Perfect Types enforced:
  `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-src 'self'; worker-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'; require-trusted-types-for 'script'; trusted-types 'none'`
  (add `upgrade-insecure-requests` behind TLS).
- `/sandbox-frame`:
  `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'; sandbox allow-scripts; frame-ancestors 'self'`
  (Perfect Types **omitted here** so the bootstrap may set `srcdoc` and the renderer may use
  `innerHTML` — safe because the frame is opaque-origin + `connect-src 'none'`).
- `/api/*`:
  `default-src 'none'; frame-ancestors 'none'; require-trusted-types-for 'script'; trusted-types 'none'`.

**Common headers (every response):** `X-Content-Type-Options: nosniff`; `X-Frame-Options: DENY`
(`SAMEORIGIN` only for `/sandbox-frame`); `Referrer-Policy: no-referrer`;
`Cross-Origin-Opener-Policy: same-origin`; `Cross-Origin-Embedder-Policy: require-corp`;
`Cross-Origin-Resource-Policy: same-origin`; `Document-Isolation-Policy: isolate-and-require-corp`;
`Permissions-Policy:` deny-all list; `X-Permitted-Cross-Domain-Policies: none`;
`Cache-Control: no-store`.

**Hard invariant — strict cross-origin isolation on every response.** COOP `same-origin`, COEP
`require-corp`, CORP `same-origin` (the strictest standard values) are set unconditionally on
*every* route class, plus DIP `isolate-and-require-corp`. Together they make the document
cross-origin-isolated in its own process (anti-Spectre) and enable the strongest iframe isolation.
Never relaxed — everything coven-web serves is same-origin, so the strictest values always apply.
(Coven's JSON API sits behind the same-origin proxy, so the browser never loads it cross-origin.)

**MIME types** (under `nosniff`, wrong type = refusal to execute): `.js → text/javascript;
charset=utf-8`, `.css → text/css; charset=utf-8`, `.svg → image/svg+xml`, `.json →
application/json`, `.map → application/json`.

**CSRF (v2 writes):** `__Host-` session cookie, `SameSite=Strict`, plus a `sec_fetch_csrf`
middleware that rejects cross-site state-changing requests (reads `Sec-Fetch-Site/Mode/Dest`).
Bearer/non-browser clients exempt. v1 is read-only, so the layer is scaffolded but a no-op.

## 6. Crypto direction (foundational; drives WS-A and WS-H)

`std/crypto` grows by **modeling Go's `crypto/*` packages**, expanding as features demand, and
**preferring FIPS-approved algorithms**: FIPS 186-5 signatures (ECDSA, EdDSA), FIPS 180-4 / FIPS 202
hashes (SHA-2 / SHA-3), FIPS 198-1 HMAC, FIPS 197 + SP 800-38D AES-GCM, SP 800-90A DRBG. SHA-1/MD5
excluded for new signature/MAC use.

**Backend decision: standardize witchy's native crypto on `aws-lc-rs`** (the FIPS-capable AWS-LC
binding) as the single backend, replacing today's RustCrypto crates (`sha2 0.11`, `ed25519-dalek
2.2`, and the `getrandom`-based keygen). One provider supplies the whole target surface — SHA-2/3,
HMAC, ECDSA P-256/384, Ed25519, AES-GCM, RSA, DRBG — and its `fips` feature is the path to actual
140-3 validation later (a separate future effort, not this initiative). Tradeoff: aws-lc-rs builds
AWS-LC (C/asm), so a C toolchain/cmake (or a prebuilt) joins the build. Crypto sits **entirely
host-side** (a Rust host import in `src/native.rs`, never compiled into guest WASM), so the WASM
backend is unaffected. Private/signing keys stay host-minted, unforgeable `SigningKey`
*capabilities*; hashing/verification stay pure (no capability).

Distinction to keep honest: *using FIPS-approved algorithms* (near-term goal) vs. formal **FIPS
140-3 CMVP module validation** (separate/heavy; not in scope here).

## 7. Workstreams

Each is an implementable unit with deliverables and acceptance criteria. Dependencies noted.

### WS-A — Crypto backend: migrate `std/crypto` native impls to `aws-lc-rs`
*Depends on: none. Foundational; can start immediately and in parallel.*
- Replace `sha2` + `ed25519-dalek` in `src/native.rs` with `aws-lc-rs` (pin latest). Keep the
  existing `std/crypto` witchy surface (`sha256`, `ed25519_verify`, `sign`, `public_key`,
  `rune_hash`) byte-for-byte compatible (the pm/coven content-hash + signed-record formats must
  not change — verify against committed example locks and the Rust verifier tests).
- Add, Go-modeled: **ECDSA P-256 / ES256** (FIPS 186-5; COSE alg -7), **HMAC-SHA256** (FIPS 198-1),
  **SHA-512/384** (FIPS 180-4), **SHA-3** (FIPS 202). Keep Ed25519/EdDSA (FIPS 186-5; COSE alg -8).
- Add a crypto-grade **`rand`** (SP 800-90A DRBG) behind an `Entropy` capability (today
  `std/random` is a non-crypto LCG — leave it, but mark it unfit for keys).
- *Acceptance:* whole suite green (incl. pm/coven signing/hashing golden tests) on aws-lc-rs;
  `caps_audit`/`caps_guard`/`coven_check` unaffected; new primitives unit-tested against known
  vectors; `cargo build` documented C-toolchain requirement.

### WS-B — `std/server` hardening
*Depends on: none. Prereq for serving real content.*
- **B1 (required):** `Content-Length` must be **byte** length, not `string.length` char count
  (`std/server.witchy:379`). Add a `string` byte-length op if absent. Until fixed, any multibyte
  body (proxied package source) is mis-framed.
- **B2:** a `serve_file(dir, path, content_type)` helper with strict path validation (reuse coven's
  name/`..` rejection); serve an explicit asset allowlist, not a generic file server.
- **B6 (proxy resilience — found in M1):** `connect` raises a *fatal* runtime error on a failed
  dial (`src/interpreter.rs:1347`, `src/runtime.rs:1069`), so a down upstream **crashes the whole
  server** (std/server has no per-request trap isolation; witchy traps are unrecoverable). Add a
  fallible dial — e.g. `net.try_connect -> Option(Socket)` — across the interpreter, the WASM host
  import, typeck, and a `std/http`/`std/server` wrapper, so the proxy returns **502** instead of
  dying. Shared-tree (touches the compiler) → coordinate with the active agent.
- *Acceptance:* a handler returning a UTF-8 body with multibyte chars is received intact; static
  assets serve with correct MIME; `../` traversal rejected; the proxy returns 502 (not a crash)
  when coven is unreachable.

### WS-C — `coven-web` witchy server
*Depends on: WS-B.*
- New program `projects/coven-web/src/coven_web.witchy`. Capabilities at `main`: `Net[Listen]`,
  `Net[Connect]`, `Dir[Read]` — nothing else (the server's own footprint is a coven demo).
- Routes: `/`+`/index.html` (shell), `/app.js`, `/styles.css`, `/sandbox-frame` (bootstrap),
  `/source-sandbox.js`, `/highlight.min.js`, `/api/coven/*` (proxy).
- **Reverse proxy (v1):** GET allowlist exactly `{rootpub,index,versions,record,source,snapshot,
  timestamp}`; `http.get(net, coven_host, coven_port, "/coven/"+rest)`; return `json(status, body)`
  with our own headers (don't forward coven's). Reject non-allowlisted paths (anti-SSRF). Pass
  `name`/`version` verbatim. Normalize the `/coven/rootpub` text-vs-JSON shape.
- **Middleware (two `.layer()`s):** `security_headers` (CSP-by-path-class + common headers, §5.3)
  and `sec_fetch_csrf` (no-op in v1).
- *Acceptance:* every security header present + exact per route class; proxy returns coven data
  same-origin; SSRF allowlist enforced; MIME correct.

### WS-D — Zero-dep TS client foundation
*Depends on: WS-C (something to serve). Build is independent.*
- `projects/coven-web/web/`: `package.json` `dependencies: {}`; vendored, pinned toolchain in
  `web/tools/` (node, esbuild, tsc, oxlint) via `tools.json`; `oxlintrc.json` (`no-eval` etc.);
  strict `tsconfig.json` (`noEmit`, the real gate).
- `src/security/escape.ts` (`SafeHtml` tagged template + `rawHtml`); `src/ui/dom.ts` (the only
  HTML-writing module, via `setHTML(SafeHtml)`); `src/main.ts` (wires it; creates **no** TT policy);
  `src/api.ts` (same-origin fetch to `/api/coven/*`, `~`-encode names, typed Record).
- `dist/index.html` (hand-edited, not built); esbuild IIFE bundle → `dist/app.js`.
- *Acceptance:* `tsc` strict passes; a deliberate `el.innerHTML = s` throws at runtime in the shell;
  bundle loads with the app-shell CSP and no console violations.

### WS-E — Read-only views
*Depends on: WS-C, WS-D.*
- **Index/search** (`/api/coven/index`, client-side filter), **Rune detail** (`/api/coven/versions`,
  state badges + footprint chips + `released_at`), **Version detail** (`/api/coven/record`: hash,
  footprint, determinism, provenance, signature shown), **Capability footprint panel** (marquee:
  what authority the rune demands). Parent-only escaped DOM.
- *Acceptance:* browse a real local coven end-to-end (index → rune → version → footprint).

### WS-F — Sandboxed source viewer (security centerpiece)
*Depends on: WS-C, WS-E.*
- Double-iframe + `/sandbox-frame` bootstrap + `MessageChannel` handshake (§5.2); file tree +
  selected file from `/api/coven/source` rendered **inside the sandbox** with highlight.js
  (vendored, executes only in-sandbox); implement the render RPC.
- *Acceptance:* package source renders highlighted; a `fetch()` attempted inside the sandbox is
  blocked by `connect-src 'none'`; opaque origin confirmed (no `allow-same-origin`).

### WS-G — Trust panel
*Depends on: WS-E (+ optionally WS-A for client-side verify).*
- `/api/coven/snapshot` + `/timestamp` + `/rootpub`: TUF freshness (expiry/rollback), root-key
  fingerprint, per-record signature status. **Optional:** client-side ed25519 verification — the
  first witchy-WASM-in-browser module (pure verify, no DOM; needs the WS-I host shim).
- *Acceptance:* a tampered/rolled-back/expired state is visibly flagged.

### WS-H — Promote ("2FA to publish") write flow
*Depends on: WS-A (crypto), WS-C (proxy POST), WS-D. v2.*
- **B3:** `std/cookie` (parse `Cookie`; format `Set-Cookie` `__Host-`/`Secure`/`HttpOnly`/`SameSite=Strict`).
- **B4:** WebAuthn second factor — prefer **ECDSA P-256 / ES256** (dominant FIDO2 algorithm, from
  WS-A), Ed25519/EdDSA fallback; add a small **pure-witchy CBOR/COSE + authenticatorData/
  clientDataJSON** parser (`std/webauthn.witchy`). (`crypto.hmac_sha256` enables TOTP as an
  alternative factor.)
- Activate `sec_fetch_csrf`; add a POST allowlist (`promote`,`yank`) to the proxy. UI: maintainer
  login → **staged queue** → review **footprint delta** (`delta_runtime`/`delta_build`, preview by
  diffing staged vs latest-released) → second-factor ceremony → `POST /api/coven/promote`. The
  session brokers obtaining/relaying coven's `id_token`.
- *Acceptance:* a staged version is promoted from the browser with a real second factor; coven's
  separation-of-duties (self-promote) rejection surfaces; Sec-Fetch CSRF rejects a forged cross-site
  POST.

### WS-I — Framework rune (north star)
*Depends on: a clean WS-F render seam. Don't front-load; extract from real patterns.*
- **Design:** specified in [RFC-0008](../../rfcs/0008-frontend-framework-rune.md), depending on
  [RFC-0006](../../rfcs/0006-compile-time-tagged-literals.md) (compile-time `html`) and
  [RFC-0007](../../rfcs/0007-witchy-wasm-browser-target.md) (the pure-compute browser target).
- **B5:** a **browser WASM host-import shim** (JS) implementing witchy's `"witchy"` import ABI
  (string-bridge + `encoding`; **deny** all capability imports → structurally I/O-incapable).
- The framework: pure `view(state) -> VNode` / `update(state, msg) -> state` as a rune with a
  **provably empty footprint** (verified by coven's analyzer). A thin TS shell diffs `VNode` → DOM,
  marshals events back as `msg`. Migrate coven-web's sandbox highlighter, then renderer, onto it;
  publish the framework **to coven** as the proof.
- *Acceptance:* `compiler.footprint` reports an empty footprint for the framework rune; coven-web
  renders through it with the trusted parent's TS surface no larger than before.

## 8. Milestones (ordering across workstreams)

- **M0 — Foundations (parallel):** WS-A (aws-lc-rs migration) ‖ WS-B (`std/server` B1/B2).
  *Done when:* suite green on aws-lc-rs; multibyte bodies framed correctly; static serving works.
- **M1 — Server shell:** WS-C + WS-D. *Done when:* the SPA shell loads same-origin with the full,
  correct header stack and a stray `innerHTML` throws.
- **M2 — Read-only browse:** WS-E. *Done when:* a real coven is browsable index→rune→version→footprint.
- **M3 — Sandboxed source:** WS-F. *Done when:* source renders in the sandbox and network is firewalled.
- **M4 — Trust:** WS-G. *Done when:* tamper/rollback/freshness is surfaced.
- **M5 — Promote/2FA:** WS-H. *Done when:* browser-driven promote with a real second factor works,
  SoD + CSRF rejections verified.
- **M6 — Framework rune (north star):** WS-I.

## 9. Testing & acceptance

- **Server e2e** (`tests/e2e.rs`-style): spawn coven + coven-web; assert exact header strings per
  route class, proxy correctness, MIME, asset path-traversal rejection, proxy SSRF allowlist.
- **CSRF (v2):** Sec-Fetch rejection cases; bearer-exempt; non-browser (no Sec-Fetch) passthrough.
- **Sandbox isolation:** opaque origin; `connect-src 'none'` blocks network; no `allow-same-origin`;
  no top-navigation.
- **Perfect Types:** a parent `innerHTML = string` throws; `setHTML` is the only HTML-insertion path.
- **Crypto (WS-A):** known-answer vectors for each new primitive; pm/coven signing+hashing golden
  tests unchanged; Rust verifier interop preserved.
- **Client:** `tsc --noEmit` strict (the gate) + oxlint (non-blocking). **Browser walkthrough
  required** (golden path + edge cases) + Playwright snapshots per view.
- **Threat-model doc:** `projects/coven-web/SECURITY.md` — sandbox guarantees, what never enters a
  sandbox, accepted residual (side-channel) risk.

## 10. Isolation & coordination

- coven-web is a **separate program** depending on coven only via the §4 contract. No edits to
  `projects/coven/` or `src/pm/`.
- Shared-tree changes are limited to **WS-A** (`src/native.rs` + `Cargo.toml`) and **WS-B**
  (`std/server.witchy` + maybe a `string` byte-length op). Land each as a small, isolated commit;
  coordinate timing with the other active agent. Commit as insanitybit; do not push unless asked.

## 11. Open decisions (resolve at kickoff if needed)

- aws-lc-rs build: depend on the system C toolchain/cmake vs. a prebuilt — pick per CI constraints.
- WebAuthn factor algorithm priority (ES256 vs Ed25519) — default ES256 for authenticator coverage.
- Single-process consolidation (router `.nest`) timing — deferred until coven settles.

## Implementation log

- **2026-06-14 — M1 + minimal M2 verified.** `projects/coven-web/` scaffolded as a separate witchy
  `std/server` program (`src/coven_web.witchy`, `witchy.toml`, `web/dist/{index.html,app.js,styles.css}`).
  Serves the app shell, `/app.js`, `/styles.css`, the `/sandbox-frame` bootstrap, and
  reverse-proxies the seven coven read endpoints same-origin. The full per-route header stack
  (Perfect Types app CSP, sandbox CSP, COOP/COEP/DIP, X-Frame-Options, Permissions-Policy, nosniff,
  no-referrer, no-store, correct MIME) is applied by a `.layer()` middleware and verified on the
  wire. A placeholder zero-dep client (`createElement`/`textContent` only — no `innerHTML`) fetches
  `/api/coven/index` through the proxy and renders the rune list; confirmed in a real browser
  (Playwright) with zero CSP violations (only a benign `favicon.ico` 404).
  Run: `witchy sandbox --dir projects/coven-web/web/dist --net 127.0.0.1:8080 --net 127.0.0.1:8787 projects/coven-web/src/coven_web.witchy 127.0.0.1:8080 127.0.0.1:8787`
  (point arg 2 at a live coven; e.g. seed a fake registry tree for `/index`).
- **2026-06-14 — M3 (sandboxed source viewer mechanism) verified.** Added `/source-sandbox.js`
  (in-sandbox renderer, served as text) and the client sandbox host. Verified in a real browser:
  the **double-iframe** (outer `/sandbox-frame` → inner `srcdoc`, both opaque-origin) renders
  untrusted source via a `MessageChannel` handshake (ready → render → height), and an in-sandbox
  `fetch('/api/coven/index')` is **blocked by `connect-src 'none'`** (console CSP violation at
  `about:srcdoc`, status line "BLOCKED ✓"). The security centerpiece works end-to-end. (The
  placeholder `<pre>` renderer was later replaced with real, **zero-dep witchy syntax
  highlighting** — a self-contained tokenizer in `source-sandbox.js` that builds spans via
  `createElement`/`textContent`, never `innerHTML` of source, and is round-trip-tested. A
  witchy-WASM highlighter remains a WS-I aspiration.)
- **2026-06-14 — WS-E (read-only detail views) verified.** Placeholder client now navigates
  index → rune versions (state badges + capability-footprint chips; the `Console`→`Console,Net`
  widening across acme/money 1.0.0→2.0.0 is visible) → version record detail (hash, footprint,
  determinism, provenance, uploaded/promoted-by, second-factor, release date, signature), each
  version carrying the sandboxed source viewer. Verified by clicking through in a real browser
  (seeded `coven.json` records under a fake registry; `/versions` + `/record` return them since
  reads aren't signature-verified server-side). All DOM via createElement/textContent.
- **2026-06-14 — WS-D (real zero-dep TS client + vendored toolchain) done.** Replaced the
  placeholder with strict TypeScript under `web/src/` (types, api, dom, widgets, sandbox, views,
  main). The trusted parent builds DOM only with createElement/textContent (no HTML-string sink at
  all → strongest Perfect Types posture). Build tools vendored + pinned under `web/tools/`
  (esbuild 0.27.4, typescript 5.9.3, oxlint 1.56.0; `web/tools.json`); `web/package.json` runtime
  deps `{}`. `web/build.sh` runs oxlint (0 errors) → `tsc --noEmit` (passes, the gate) → esbuild
  bundle → `dist/app.js` (7.4kb IIFE). Re-verified in-browser: identical behavior (index, detail
  views, sandbox BLOCKED ✓).
- **Open finding:** B6 (proxy crashes when coven is unreachable) — see WS-B. Blocked on
  `src/runtime.rs` (currently dirty — the active agent is in it).
- **2026-06-14 — WS-G (trust panel) done.** A `views/trust.ts` view fetches `/api/coven/{rootpub,
  snapshot,timestamp}` and renders the real, Ed25519-signed TUF state: root-key fingerprint,
  snapshot version + target count + signature, timestamp expiry with a **freshness (freeze) check**,
  and a **rollback check** (timestamp.snapshot_version == snapshot.version). Verified in-browser
  against live metadata (coven's `h_snapshot`/`h_timestamp` sign on-demand from the store, so the
  seeded registry yields real signed roles — no publish needed). Reached via a "Trust & integrity"
  link on the index.
- **2026-06-14 — strict cross-origin isolation locked in (user directive).** COOP `same-origin`,
  COEP `require-corp`, CORP `same-origin` (strictest standard values) + DIP `isolate-and-require-corp`
  verified present on **all 7 route classes**; marked a HARD INVARIANT in `coven_web.witchy` and §5.3.
- **2026-06-14 — WS-F (real sandboxed source viewer) done.** Drove a real anonymous publish to the
  running coven (`demo/greeter` 1.0.0, pure `[]` footprint → staged with a real content hash + signed
  record + stored source; coven recomputes the footprint and rebuilds TUF on publish). Wired the
  version view to fetch the real `/api/coven/source` (graceful "unavailable" fallback for records
  without stored source). Verified in-browser: `demo/greeter@1.0.0` shows its real hash, real 128-hex
  Ed25519 signature, "no authority" footprint, and its **actual source** rendered inside the sandbox
  with network **BLOCKED ✓**. (ASCII source sidesteps B1; multibyte still needs the byte-length fix.)
- **Verified milestones — the ENTIRE read-only frontend is done on real signed registry data:** M1
  (secure shell + headers + proxy), M2 (index), M3 (sandbox + network firewall), WS-E (detail views),
  WS-D (typed build), WS-G (trust panel), WS-F (real source). 100%-witchy server + zero-dep typed
  client + strict cross-origin isolation everywhere.
- **2026-06-14 — WS-H frontend (promote "2FA to publish" + yank + CSRF) done.** Added POST proxy
  routes (`/api/coven/promote`, `/api/coven/yank`) and a **Sec-Fetch CSRF `.layer()`** to
  `coven_web.witchy`, plus a promote/yank UI in the version view. Verified in-browser: a staged
  `demo/logger@1.0.0` (Console footprint) promoted **staged → released** through the browser —
  second factor accepted, separation of duties enforced by coven (promoter `maintainer@demo` ≠
  uploader `ci@demo`), record re-signed with a release timestamp, view refreshed to a Yank control.
  CSRF proven both ways: cross-site & same-site writes → **403**, same-origin & non-browser → 200.
  Only gap: the second factor is a trusted string; **real WebAuthn verification needs WS-A crypto**.
- **2026-06-14 — e2e verification harness done (§9/§11).** `projects/coven-web/verify.py` spawns a
  real coven + coven-web on throwaway ports, seeds a publish, and asserts the whole contract: Perfect
  Types CSP, strict COOP/COEP/CORP on every route class, MIME, the sandbox-frame's own
  `connect-src 'none'` CSP + opaque-origin guard, proxy correctness, the **anti-SSRF allowlist**
  (non-allowlisted `/api` → 404), and the **Sec-Fetch CSRF layer both ways** (cross-site/same-site →
  403, same-origin → 200), the **promote/2FA write path** (empty-factor → 400, self-promote → 403
  by separation of duties, staged→released → 200), and a **Unicode source round-trip** (raw UTF-8).
  **All 25 checks PASS** (expanded 2026-06-14). (B2 "serve_file" is functionally satisfied inline:
  coven-web serves a fixed per-asset route allowlist with `exists` guards — no generic file server, no
  traversal surface — so a std helper isn't required for safety.)
- **THE WHOLE FRONTEND IS DONE & VERIFIED ON REAL DATA** — read (index/rune/version/footprint/
  source/trust) and write (promote/yank) — on a 100%-witchy server + zero-dep typed client, strict
  Perfect Types + cross-origin isolation, sandbox-contained source, Sec-Fetch CSRF, with a passing
  e2e harness.
- **2026-06-14 — frontend polish.** Added the index **search/filter** (PLAN §7.1 — was the one WS-E
  gap; verified narrowing the list live), a **per-file source selector** (WS-F §7.4 "file tree +
  selected file"; verified switching witchy.toml / src/*.witchy in the sandbox), and suppressed the
  favicon 404 (`<link rel="icon" href="data:,">`). Console is now clean except the intended sandbox
  `connect-src 'none'` blocks.
- **2026-06-14 — acted on the "you can modify `src/`" green-light; scoped every backend item.** The
  tree builds (`cargo check` clean, 1m35s), so verifiable `src/` work in non-async files is possible.
  Findings on why each backend item is still NOT safe to rush mid-async-refactor:
  - **WS-A (aws-lc-rs) is entangled.** `native.rs` is **not** cfg-gated off wasm32 (`lib.rs:30`), and
    aws-lc-rs has no wasm32 support → it would break the wasm playground build (which I can't easily
    verify here). The crypto host **bridge lives in `runtime.rs`** (`:491-492,576` — an async-dirty
    file I must not touch), and crypto spans 6 files (native/runtime/main/pm·keys/tuf/store). A clean
    single-backend migration needs the async work landed first. `cmake` is now installed for then.
  - **WS-B B6 (fallible connect)** needs `runtime.rs`'s `host_net_connect` (async-dirty) for the WASM
    path coven-web uses → blocked on async.
  - **`std/json` `\u` fix** touches only clean files but needs a both-backend `char_from_code`;
    codegen *can* build it (the `int_to_string` `[len][bytes]` pattern, codegen.rs:7855+), but it's
    substantial WAT with real WASM-backend regression risk — unwise to land mid-refactor where a
    miscompile would be blamed on / tangled with the async changes.
  Decision: defer all three until `ASYNC_DONE.md`; everything safe + verifiable is already done.
- **2026-06-14 — ASYNC_DONE.md fired; WS-A (aws-lc-rs crypto) DONE + verified.** With the async
  work settled (uncommitted but stable; `cargo check` green), migrated the native `crypto` module
  (`src/native.rs`: sha256/rune_hash/ed25519_verify/sign/public_key) to **aws-lc-rs** via cfg-split
  helpers — native uses aws-lc-rs (FIPS-approved SHA-256 + Ed25519), wasm32 keeps the untouched
  RustCrypto path (aws-lc-rs has no wasm32 support; gated in `Cargo.toml` under
  `cfg(not(target_arch="wasm32"))`). Did **not** touch `runtime.rs` (the crypto host bridge calls
  `native::lookup`, interface unchanged) or any async file. Verified: `cargo build` clean (aws-lc-rs
  v1.17 + AWS-LC built via cmake, 2m); **`crypto_sha256_matches_known_vectors` (KAT)** + ed25519
  verify/sign + rune_hash + the **WASM-backend** crypto tests all pass; the **cross-compat golden
  tests** `coven_witchy_signed_record_verifies_under_the_rust_verifier` and `..._tuf_metadata_...`
  pass (aws-lc-rs sigs verify byte-identically under the Rust ed25519-dalek verifier); clippy clean
  in native.rs; aws-lc-rs confirmed absent from the wasm32 build. Follow-ups: new primitives
  (ECDSA P-256/HMAC/SHA-512/SHA-3/DRBG) and optionally migrating pm/keys·tuf·store + runtime's own
  sha2 to aws-lc-rs for a single backend. **Uncommitted** (alongside the async work; commit
  selectively: `src/native.rs`, `Cargo.toml`, `Cargo.lock`).
- **2026-06-14 — WS-B B6 (fallible connect / proxy resilience) DONE + verified.** Added a total
  `net.try_connect(addr) -> Option(Socket)` (`interpreter.rs` + `typeck.rs` `check_net_op`, same
  `Net[Connect,Tcp]` gating as `connect`; `codegen.rs` emits a clear interpreter-only error — WASM
  support is a follow-up), `std/http` `try_request`/`try_get`/`try_post -> Result(Response,String)`,
  and the coven-web proxy now returns **502** (not a crash) on a down upstream. Verified: kill coven →
  proxy 502 and the **server survives**, then **recovers** when coven returns; `verify.py` asserts B6
  (**27 checks pass**); net+http suites green (no regression). **Run-mode change:** since `try_connect`
  is interpreter-only for now, coven-web runs on the **interpreter** —
  `cd projects/coven-web/web/dist && witchy --net <web> --net <coven> ../../src/coven_web.witchy <web> <coven>`
  — not `witchy sandbox`. Caps (Net allowlist, Dir=cwd) still enforced; only WASM-VM memory isolation
  is traded (fine for trusted first-party server code — coven itself runs interpreted). Uncommitted:
  `src/{interpreter,typeck,codegen}.rs`, `std/http.witchy`, `projects/coven-web/{src/coven_web.witchy,verify.py}`.
- **2026-06-14 — WS-A new primitive: `crypto.ecdsa_p256_verify` (WebAuthn ES256) DONE + verified.**
  Added to native.rs (aws-lc-rs `ECDSA_P256_SHA256_ASN1`; native-only `#[cfg(not(wasm32))]` — not
  bridged to WASM, WebAuthn runs interpreted), registered in `native::lookup`, stub in
  `std/crypto.witchy`. Takes hex SEC1-uncompressed pubkey + raw message + hex ASN.1-DER sig → Bool;
  total. Verified against a real KAT (cryptography-lib vector): valid→true, tampered→false,
  malformed→false; durable Rust test `crypto_ecdsa_p256_verify_checks_signatures` passes.
- **Remaining — honest scope:**
  - **WS-H real WebAuthn is the big blocker, and it's blocked on missing infrastructure, not just
    size.** Assertion verification needs to parse **binary** structures — `authenticatorData`
    (rpIdHash‖flags‖counter…) and, for registration, the CBOR `attestationObject` → COSE public key.
    witchy has **no binary/byte primitives** (strings are UTF-8; can't slice arbitrary bytes), so a
    correct pure-witchy WebAuthn parser is blocked until byte primitives + a CBOR reader exist. The
    crypto half (ES256 verify) is now done; the parsing half is a substantial, security-critical
    effort that should NOT be rushed. Interim: the promote flow already works with a trusted-string
    second factor (verified); upgrading it to real WebAuthn is the remaining work.
  - **WS-A primitives — `crypto.sha512` + `crypto.hmac_sha256` DONE too (2026-06-14).** aws-lc-rs,
    native-only, KAT-verified (SHA-512("abc"); HMAC-SHA256 RFC 4231 #1); stubs + docs updated; test
    `crypto_sha512_and_hmac_match_known_vectors` passes. New-primitive set now: ES256, SHA-512,
    HMAC-SHA256. **SHA3-256 also DONE (2026-06-14)** — aws-lc-rs `SHA3_256`, native-only, verified
    vs the FIPS 202 KAT (`sha3_256("abc")`); stub + docs updated. (No Rust test added — main.rs is
    being actively edited by the await agent; verified via a witchy run instead.) New-primitive set:
    ES256, SHA-512, SHA3-256, HMAC-SHA256. DRBG/`Entropy` still open (needs interpreter/typeck — see below).
  - **Follow-ups CLEARED once the postfix-`await` migration landed (`ASYNC_DONE.md`, 2026-06-14).**
    With the tree settled, the previously-blocked items are DONE:
    - **WASM-codegen `try_connect`** — a non-trapping `net_try_connect` host import returns a `-1`
      sentinel on a failed dial; codegen wraps it as `Option(Socket)` (`Some(handle)`/`None`). Verified
      interpreter↔WASM parity for both the refused-port (`None`) and live-listener (`Some`) paths.
    - **All aws-lc-rs crypto bridged to WASM** — `ecdsa_p256_verify`, `ecdsa_p256_verify_hex`, `sha512`,
      `sha3_256`, `hmac_sha256`, and `encoding.base64url_of_hex` now have host imports (the host runs the
      SAME native registry the interpreter uses). With these + `try_connect`, **coven-web runs FULLY in
      the WASM sandbox** (zero interpreter-only features): `verify.py` is 30/30 with the server launched
      under `witchy sandbox` (cross-origin isolation + WebAuthn 2FA + B6 all exercised through WASM).
      Locked in by `crypto_extensions_backends_agree`.
    - **std/json `\u` decoder fixed** — added a general `string.from_code(Int) -> String` primitive
      (native + typeck + a `string_from_code` WASM host bridge); `scan_string` now decodes `\uXXXX`
      including UTF-16 surrogate pairs for astral characters. Verified on both backends; locked in by
      `string_from_code_backends_agree` + `json_unicode_escapes_backends_agree`. `spec/stdlib.md`
      regenerated (`from_code` documented; `stdlib_docs_are_current` passes).
    - **DRBG/`Entropy` — intentionally NOT built.** The WebAuthn challenge is derived as an Ed25519
      signature over a timestamp under the held `Secret`; that is unforgeable/unpredictable to anyone
      without the key, so it is already a sound challenge. A dedicated entropy capability would be a
      stylistic nicety, not a security requirement — out of scope per "don't add features beyond what
      the task requires."
  - **Frontend is 100% complete and verified.**
- **2026-06-14 — full test suite GREEN after all backend changes.** `cargo test`: 268 lib + 852 main
  tests, **0 failures** (WS-A aws-lc-rs, B6 try_connect, ES256, all clean). Regenerated `spec/stdlib.md`
  (`witchy doc std/*.witchy`) to document the new functions (`crypto.ecdsa_p256_verify`,
  `http.try_get/try_post/try_request`) — `stdlib_docs_are_current` passes. So everything implemented so
  far is fully verified end-to-end. (I initially thought WS-H was blocked on a witchy `Bytes` type +
  CBOR reader for binary parsing — that turned out to be wrong; see the next entry.)
- **2026-06-14 — WS-H server-side WebAuthn verification CORE DONE + verified (the "blocked" item,
  unblocked).** Insight: the binary-parsing blocker **dissolves if the browser hex-encodes the
  ArrayBuffers** (authenticatorData, signature) at the boundary — coven then verifies entirely with
  **text/hex string ops + crypto**, no `Bytes` type needed, while still checking everything
  INDEPENDENTLY (trusting nothing from the client). Added `crypto.ecdsa_p256_verify_hex` (hex
  message) and **`std/webauthn.witchy`** `verify_assertion(...)`, which checks clientDataJSON
  type/challenge(replay)/origin(phishing), authenticatorData rpIdHash(wrong-RP)/UP/UV flags, and the
  ES256 signature over `authData‖SHA256(clientData)`. Verified against **two real P-256 vectors**
  (UV=1/UV=0): valid→ok; wrong-challenge, wrong-origin, wrong-rpId, tampered-sig, missing-UV all
  rejected (8/8). Durable test `webauthn_verify_assertion_checks_an_es256_assertion` passes; full
  suite green (268+854+47, 0 fail); `webauthn` registered as a std module; docs regenerated.
  **Remaining WS-H = integration only (no crypto/parsing blockers left):** the browser ceremony
  (`navigator.credentials.create/get`, CBOR pubkey extraction at registration, hex-encoding — all
  natural in TS), coven challenge issuance + per-maintainer credential storage, and wiring
  `verify_assertion` into the promote second-factor gate (replacing the trusted string).
- **2026-06-14 — WS-H integration foundations DONE.** Added `encoding.base64url_of_hex` (base64url
  of bytes-given-as-hex — needed because `clientDataJSON.challenge` is base64url; a real stdlib gap,
  KAT-tested: `hex("test-challenge")` → `dGVzdC1jaGFsbGVuZ2U`). All WS-H *dependencies* now exist:
  ES256 verify (`ecdsa_p256_verify_hex`), SHA-256, the `verify_assertion` core, base64url. Full suite
  green (268+855+47, 0 fail). **The remaining WS-H work is integration wiring only** (no novel/crypto
  pieces): (a) browser ceremony in TS — `navigator.credentials.create` (get the pubkey via
  `getPublicKey()`: SPKI→SEC1 last 65 bytes, **no CBOR**) + `.get` + hex-encode; (b) coven-web server
  endpoints — `/api/webauthn/register` (store credentialId+pubkey in an unrouted `_wa_*` file),
  `/api/webauthn/challenge` (derive a single-use challenge from a `Secret` via `crypto.sign` + Clock,
  base64url it), `/api/coven/promote-2fa` (call `verify_assertion`, then forward to coven's promote);
  this cascades into coven-web's run model (grant `Secret`+`Clock`, pass origin/rpId) and `verify.py`.
  (c) a Playwright **virtual-authenticator** e2e. Design is fully specced; it's standard webapp
  plumbing on top of the verified crypto core.
- **2026-06-14 — WS-H COMPLETE, verified END-TO-END in a real browser.** Built the full integration:
  (1) **coven-web endpoints** `/api/webauthn/register` (store credentialId+pubkey in unrouted
  `_wa_*.json`), `/api/webauthn/challenge` (single-use, derived from the `Secret` via `crypto.sign`
  + Clock, base64url'd), `/api/coven/promote-2fa` (verify via `std/webauthn`, then forward to coven);
  coven-web now takes `Secret`+`Clock`+origin/rpId. (2) **Browser ceremony** `web/src/webauthn.ts`
  (`navigator.credentials.create` resident passkey, pubkey via `getPublicKey()`→SEC1, no CBOR; `.get`;
  hex-encode) wired into the staged-version UI ("Register passkey" / "Promote with passkey (2FA)").
  (3) **`verify.py` extended to 30 checks** incl. register + valid-assertion-promotes + tampered-403,
  all PASS. (4) **Real-browser e2e** (Playwright CDP virtual authenticator): register → assert →
  promote `demo/pk` → coven independently reports **`released`**; UI shows released + Yank. **Found +
  fixed a real bug**: the deny-all Permissions-Policy blocked WebAuthn — now
  `publickey-credentials-get/create=(self)` (self only, not delegated to the sandbox iframes). The
  promote 2FA gate now uses a real ES256 passkey, verified server-side. (Run-model note: coven-web
  needs `--signing-key` + origin/rpId args; the `_wa_*.json` state lives unrouted in the served Dir,
  gitignored.) **The plan's headline security feature — real WebAuthn 2FA for promote — is done.**
- **2026-06-14 — B1 is a NON-ISSUE (disproven).** `string.length` returns **byte** length (UTF-8) —
  `std/string.witchy:12-15` documents it, `interpreter.rs:849` is `s.len()`. So `render_response`'s
  `Content-Length` was already byte-correct. Verified by round-tripping multibyte source (café/☕/
  日本語/🌍) through the proxy: with raw UTF-8 it matches **exactly**. Remove B1 from the backlog.
- **2026-06-14 — NEW BUG found: `std/json` decoder ignores `\uXXXX`.** `scan_string`
  (`std/json.witchy:285`) calls `unescape` on the single char after `\`; `unescape("u")` falls
  through returning `"u"` (`:292-302` — no `\u` case), so `é` decodes to literal `u00e9` (dropped
  backslash + 4 stray hex). Corrupts any non-ASCII in `\u`-escaped JSON coven receives — e.g.
  Python's default `json.dumps` or the Rust client's publish bodies. **Workaround: publish with raw
  UTF-8** (proven to round-trip exactly). **Proper fix needs a codepoint→char primitive that witchy
  lacks** (no `chr`/`from_code`; would be a both-backend builtin + surrogate-pair handling, with a
  hard WASM/codegen Unicode side) → deferred until `src/` settles; the encoder is fine (passes raw
  UTF-8). coven-web's source viewer itself is correct for ASCII + raw-UTF-8 runes (verified).
- **Remaining = shared-tree backend, HARD-BLOCKED by the in-flight futures/channels concurrency work**
  (another agent; broad `src/` churn incl. new `optimize.rs`). WS-A (aws-lc-rs crypto), WS-B (B1 byte
  Content-Length / B2 serve_file / B6 fallible connect), and WS-H's real WebAuthn (needs WS-A) all
  require editing/building `src/`, which would collide and be unverifiable while that lands. **The
  async work signals completion by writing `ASYNC_DONE.md` (repo root).** A persistent Monitor is
  armed to wake the loop the instant it appears; on that signal, resume WS-A → WS-B → WS-H and verify
  each (ideally in a worktree off the freshly-settled tree). Until then the frontend is fully done.

## Appendix — references for security primitives (public)

- **Perfect Types** — Jun Kokatsu, "Eliminating XSS with Trusted Types"; Frederik Braun, "Perfect
  types with `setHTML()`" (frederikbraun.de).
- **HTML Sanitizer / `setHTML`** — MDN `Element/setHTML`, `Sanitizer`; Mozilla Hacks "Goodbye
  innerHTML, Hello setHTML" (Firefox 148, 2026).
- **Trusted Types CSP** — MDN `require-trusted-types-for`, `trusted-types`; W3C Trusted Types.
- **Fetch Metadata** — `Sec-Fetch-Site/Mode/Dest` (CSRF defense-in-depth).
- **Cross-origin isolation** — COOP + COEP + Document-Isolation-Policy.
- **Crypto** — Go `crypto/*`; FIPS 186-5 (ECDSA/EdDSA), 180-4/202 (SHA-2/3), 198-1 (HMAC), 197 +
  SP 800-38D (AES-GCM), SP 800-90A (DRBG); `aws-lc-rs` (FIPS-capable backend).
- **coven design** — `rfcs/package-manager.md` (registry/pm design + built-vs-future, §15).
```
