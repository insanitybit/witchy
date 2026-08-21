---
rfc: 0136
title: "Impregnable and Ergonomic Glamour — Secure by Construction, Beautiful by Design"
status: proposed
created: 2026-08-20
superseded-by:
tracking:
predecessors:
  - "[0006](0006-compile-time-tagged-literals.md) (compile-time tagged literals)"
  - "[0008](0008-frontend-framework-rune.md) (frontend framework rune)"
  - "[0015](0015-secure-web-by-construction.md) (secure web applications by construction)"
  - "[0039](0039-glamour-capability-safe-effects.md) (Glamour capability-safe effects)"
  - "[0107](0107-glamour-next-generation-web-framework.md) (Glamour 1.0 web framework)"
---

# RFC-0136: Impregnable and Ergonomic Glamour

## Summary

Glamour established that a web framework can be capability-pure, eliminating structural XSS, ambient-cookie CSRF, and unconfined foreign code execution by construction. However, developers frequently experience friction from the "MVU ceremony tax": manually declaring message enums, wrapping no-op commands, and threading capability tokens.

This RFC introduces **Glamour 2.0**, achieving two unified goals:
1. **Beautiful by Design**: Elevating developer ergonomics with concise declarative component syntax, compile-time JSX/HTML sugar, inferred command returns, and automatic event decoders.
2. **Impregnable & Trivial to Audit**: Hardening web security by construction—eliminating XSS, CSRF, supply-chain attacks, CDN tampering, and secret leakage while making the entire authority footprint of any component auditable at a glance via `witchy caps`.

---

## 1. Ergonomic Architecture: Less Ceremony, Zero Compromise

### 1.1 Inferred `NoCmd` and Direct State Updates
In ordinary MVU, every update branch must return a tuple `(Model, Cmd(Msg))` even when no command is triggered. Glamour 2.0 permits returning either `Model` directly (inferred `(model, NoCmd)`) or `(Model, Cmd)`:

```witchy
fn update(model: Counter, msg: Msg) -> Counter:
    match msg:
        Inc -> Counter(count: model.count + 1)
        Dec -> Counter(count: model.count - 1)
```

When an asynchronous command or effect is required, returning a command tuple remains explicit:

```witchy
fn update(fetch_cap: UiFetch, model: State, msg: Msg) -> (State, Cmd(Msg)):
    match msg:
        Reload -> (State(loading: true, ..model), glamour.fetch(fetch_cap, "GET", "/api/data", DataLoaded))
        DataLoaded(res) -> (State(data: res, loading: false, ..model), NoCmd)
```

### 1.2 Inline Action Synthesis in `html"""..."""` / `jsx"""..."""`
Instead of requiring manual enum variant definitions for every trivial state update, Glamour provides compile-time action synthesis:

```witchy
fn view(model: Counter) -> Ui(Msg):
    glamour.ui(jsx"""
        <div class="counter">
            <button on:click=${|m: Counter| Counter(count: m.count - 1)}>-</button>
            <span>${model.count}</span>
            <button on:click=${|m: Counter| Counter(count: m.count + 1)}>+</button>
        </div>
    """)
```

### 1.3 Automatic Event & Form Decoders
Two-way bindings and form decoders map directly into model fields without manual JSON decoding:

```witchy
fn view(form: LoginForm) -> Ui(Msg):
    glamour.ui(jsx"""
        <form on:submit=${SubmitLogin}>
            <input type="text" value=${form.username} on:input=${bind form.username} />
            <secret-input slot="auth/password" on:ready=${PasswordCaptured} />
            <button type="submit">Log In</button>
        </form>
    """)
```

---

## 2. Impregnable Security: Immune to Vulnerabilities by Construction

Glamour provides mathematical defense-in-depth against every major category of web vulnerability:

```
┌────────────────────────────────────────────────────────────────────────┐
│                        Trusted Main Execution Plane                    │
│                                                                        │
│   Witchy Wasm Core (Capability-Free Pure Compute, Empty Host Footprint)│
│                                │                                       │
│                         Typed Patch Stream                             │
│                                ▼                                       │
│   DOM Host Shell (createElement / textContent / Strict Sink Filter)    │
│            ├── safeUrl() (Rejects C0 controls, javascript:, SVG data:) │
│            ├── Trusted Types (require-trusted-types-for 'script')      │
│            └── Host-Custodied Secrets (Passkeys, password buffers)     │
└────────────────────────────────┬───────────────────────────────────────┘
                                 │ MessageChannel
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│                  Opaque-Origin Sandbox Compartment                     │
│   <iframe sandbox="allow-scripts" csp="connect-src 'none'">            │
│   Untrusted Third-Party Code (D3, Charts, External Widgets)            │
└────────────────────────────────────────────────────────────────────────┘
```

### 2.1 Complete XSS Elimination (No Code-Data Conflation)
* **Zero HTML-String Sinks**: The runtime possesses no `innerHTML`, `outerHTML`, or `document.write` execution paths. All rendering uses `createElement`, `textContent`, and `setAttribute`.
* **Tag Allowlisting**: Elements outside `SAFE_ELEMENTS` (such as `<script>`, `<object>`, `<embed>`) fail closed at the DOM sink to inert `<span>` nodes.
* **No Inline String Handlers**: String attributes matching `^on` (e.g. `onclick="malicious()"`) are dropped; event handlers attach solely via typed message callbacks.
* **Sanitized URL Sinks**: `safeUrl()` rejects ASCII C0 control characters (`\n`, `\t`, `\r`) before scheme evaluation, neutralizing `javascript:`, `vbscript:`, protocol-relative (`//`, `/\`), and executable SVG `data:` URLs.
* **Browser Trusted Types**: The host registers the single `glamour` Trusted Types policy (`createHTML: () => { throw new TypeError(); }`), ensuring browser-enforced rejection of raw HTML injections.

### 2.2 CSRF Elimination (Zero Ambient Authority)
* **Bearer Capabilities**: Authentication uses explicit header-bearer tokens held in host custody. No ambient session cookies are transmitted automatically on cross-site requests.
* **`Sec-Fetch-Site` Enforcement**: State-changing endpoints (`POST`, `PUT`, `DELETE`, `PATCH`) require `Sec-Fetch-Site: same-origin` and fail closed on missing or cross-site headers.

### 2.3 Supply-Chain & Third-Party Code Containment
* **Foreign Code Compartments**: Third-party JavaScript cannot execute in the main page context. It must be spawned into an opaque-origin `<iframe sandbox="allow-scripts">` with `connect-src 'none'`.
* **Narrow MessageChannel Bridge**: The host communicates with the compartment solely over a structured `MessageChannel`, exchanging typed grants and serialized event messages.

### 2.4 Subresource Integrity & Content-Addressed Assets
* **Automated SRI Generation**: The compiler automatically computes and attaches SHA-384 `integrity` and `crossorigin="anonymous"` attributes to all emitted `<script>`, `<link>`, and Wasm streaming bootstrap tags.
* **Immutable Content-Addressing**: All compiled artifacts are keyed by content hash (`glamour-island-[hash].wasm`), preventing CDN poisoning and cache tampering.

### 2.5 Host-Custodied Secret Isolation
* **Zero Secret Leakage into Wasm**: Sensitive credential fields (`SecretInput`) store input strings exclusively in host memory (`dispatch.__secrets`).
* The pure Wasm application receives only opaque reference handles (`SecretRef`), preventing credential theft via memory inspection or side channels.

### 2.6 Hardware & OS Process Isolation (Anti-Spectre)
* All responses mandate strict isolation headers:
  * `Cross-Origin-Opener-Policy: same-origin`
  * `Cross-Origin-Embedder-Policy: require-corp`
  * `Document-Isolation-Policy: isolate-and-require-corp`

---

## 3. Trivial to Audit: The Capability Graph

Because Witchy enforces explicit, unforgeable capabilities, auditing an entire application's security perimeter requires no runtime fuzzing—it is verifiable from type signatures and compiler manifests:

```sh
$ witchy caps src/app.witchy
[audit] Capabilities for `app.witchy`:
  - UiFetch: ["GET /api/profile", "POST /api/settings"]
  - UiRoute: ["/dashboard/*"]
  - UiStorage: [namespace="user_prefs", max_bytes=4096]
  - HostFootprint: EMPTY (No raw Net, Dir, Clock, or Exec)
```

If a component is compromised or maliciously modified, it cannot:
1. Contact unauthorized origins (prevented by `UiFetch` method/path narrowing).
2. Execute arbitrary JavaScript (prevented by empty host capabilities and sinkless DOM).
3. Access local storage or cookies outside its granted namespace.
4. Exfiltrate secrets (prevented by host custody and capability firewalls).

---

## Acceptance Criteria

1. **Ergonomic Parity**:
   - `update` functions accept direct `Model` returns without `(Model, NoCmd)` boilerplate.
   - `jsx"""..."""` and `html"""..."""` templates support typed interpolation, inline event lambdas, and automatic form binders.
2. **Automated SRI**:
   - Production builds emit SHA-384 `integrity` hashes for 100% of generated scripts, stylesheets, and Wasm binaries.
3. **Formal Differential Hardening**:
   - Automated test suite validating that 100% of OWASP Top 10 web attack vectors fail closed and are structurally unrepresentable in Glamour.
