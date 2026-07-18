---
rfc: 0039
title: Glamour capability-safe effects and UI authority
status: implemented
created: 2026-07-01
implemented: 2026-07-02 (sealed UI caps + token-gated Http/Nav/Timer/Port + host-owned SecretInput/SecretRef; coven-web migrated as the proof; both-backend wire parity)
predecessors:
  - "0008 (frontend framework rune — the MVU core this hardens)"
  - "0007 (witchy-WASM browser target — the pure-compute rune + host shell split)"
  - "0015 (secure web by construction — compartments for foreign code)"
  - "0038 (grantable user capabilities — the root-authority mechanism this consumes)"
tracking:
---

# RFC-0039: Glamour capability-safe effects and UI authority

> Provisional syntax. Code blocks here are intentionally **not** tagged `witchy`
> so the doc-examples test does not try to compile partial snippets.

## Summary

Glamour ([RFC-0008](0008-frontend-framework-rune.md)) is a capability-*pure* MVU
core: `view(state) -> VNode(msg)`, `update(state, msg) -> (state, Cmd(msg))`, and
an empty WASM footprint. But that purity today is only about *host imports*: the
rune still exposes broad public `Cmd` constructors (`Http`, `Nav`, `Port`) and
delivers raw input values as messages, so **any** component can *describe* any
effect, and a sibling can observe a password that flows through the shared message
stream. The authority boundary lives only in the host shell's runtime policy. This
RFC gives Glamour its **own sealed, bare capabilities** (`UiRoot`, narrowed into
`UiFetch`/`UiRoute`/`UiTimer`/`SecretInput`/`CredentialPort`), makes sensitive
`Cmd`/`VNode` constructors **require** the matching token, and keeps sensitive input
bytes in host custody behind an opaque `SecretRef`. It builds directly on
[RFC-0038](0038-grantable-user-capabilities.md): the app receives `glamour.UiRoot`
at its root and Glamour narrows it to component-local tokens. Real browser authority
stays in the host shell (RFC-0007/0008); Witchy gains **no** browser capabilities.

## Motivation

Glamour's promise is *UI without ambient authority*. The rune computes inert data;
the shell acts. But statically, an app can still write

```
Cmd.Http("POST", "/api/coven/yank", body, "Done")
Cmd.Port("promote", target, "Done")
```

from *any* component, because `Http`/`Nav`/`Port` are ordinary public constructors.
Whether that call is allowed is decided by host runtime policy — not by anything in
the type. That is exactly the "authority in runtime policy, not in the type" problem
Witchy exists to avoid.

The sharpest case is **sensitive input**. `OnInput` dispatches a field's current
value into the app as an ordinary `msg` string. Once a password is app-level
message/model data, any code on the update path — including a *sibling* component —
can observe it. A component that was never given authority over another component's
password field should not be able to read it, by construction, not by convention.

We want UI authority to be as reviewable as a package footprint: a component can
only construct the effects for which it *holds a typed token*; secrets stay
host-owned; child internals are private; and the effects a rune can request are
auditable (via RFC-0038's granted-user-cap axis) so Coven can block a dependency
that widens them.

Crucially, the deeper look showed Witchy already has the mechanism — sealed user
capabilities (RFC-0002) that carry only policy data have an **empty host footprint**
and are unforgeable across modules. So this is a *Glamour* design plus the small
RFC-0038 root-grant extension — **not** new first-class browser capabilities.

## Design

### 1. Sealed, bare Glamour capabilities

All carry only policy data — no host caps — so their host footprint is empty and
they qualify as bare grantable capabilities under RFC-0038:

```
grantable capability UiRoot:          # the app-root token, granted to main
    policy: String

capability UiFetch:                   # authorizes constructing a fetch command
    scope: String
    methods: String                   # e.g. "GET" / "GET,POST"
    prefix: String                    # allowed path prefix

capability UiRoute:                   # navigation authority
    base: String
    rights: String                    # "push" / "push,replace"

capability UiTimer:
    min_ms: Int

capability SecretInput:               # authority to render a host-owned secret field
    form: String
    field: String

capability SecretRef:                 # opaque handle to a host-held secret value
    slot: String

capability CredentialPort:            # authority to invoke a named host credential op
    name: String
```

`witchy caps projects/glamour/src/glamour.witchy` must still report an **empty host
footprint** — these are policy-only tokens, not `Net`/`Dom`.

### 2. Narrowing: Glamour owns the constructors

Glamour mints child tokens from `UiRoot` *inside the framework* (it is the declaring
module, so only it may construct them — RFC-0002 sealing):

```
pub fn catalog_fetch(root: UiRoot) -> UiFetch          # GET /api/coven/*
pub fn login_scope(root: UiRoot) -> LoginScope         # SecretInput + CredentialPort
pub fn promote_scope(root: UiRoot) -> PromoteScope      # CredentialPort + UiFetch[POST /promote]
```

App composition decides which child component receives which token. The result is
an explicit, reviewable object-capability graph:

```
App(ui)
├── PackageCatalog  gets catalog_fetch(ui)          # read-only GET
├── LoginForm       gets login_scope(ui)            # secret input + login credential
└── PromoteButton   gets promote_scope(ui)          # promote credential only
```

### 3. Token-gated `Cmd` / `VNode` constructors

Sensitive variants take the token as a leading argument. Public constructors are
fine because the *authority-bearing argument is unforgeable* — the same pattern as
`read(dir: Dir[Read], path)`: public, but unusable without a `Dir`.

```
pub type Cmd(msg):
    NoCmd
    Batch(List(Cmd(msg)))
    After(UiTimer, Int, msg)
    Http(UiFetch, String, String, String, String)      # method, url, body, on_done
    Nav(UiRoute, String)
    Port(CredentialPort, String, String)               # arg, on_done

pub fn http_get(fetch: UiFetch, url: String, on_done: String) -> Cmd(msg):
    Http(fetch, "GET", url, "", on_done)
```

A component without a `UiFetch` **cannot construct a useful HTTP command** — the
unauthorized effect is unrepresentable, not merely rejected at runtime. (Public
`Cmd` constructors are acceptable precisely because the token gates them; this is
why RFC-0038, not opaque-ADT/private-constructor language surgery, is the enabling
feature.)

### 4. Sensitive input: host custody + opaque `SecretRef`

Ordinary fields keep the existing ergonomic path (`on_input` → a `String` message)
— fine for a search box. **Secret** fields do not:

```
pub fn secret_input(input: SecretInput, on_ready: String) -> VNode(msg)
pub fn submit_secret(ref: SecretRef, port: CredentialPort, on_done: String) -> Cmd(msg)
```

The host keeps the actual secret bytes; the rune receives only non-sensitive facts
(`Empty | NonEmpty | Acceptable`) or an opaque `SecretRef`. The raw password never
becomes a `String` in the message or model. A `SecretRef` may optionally be
one-shot (consumed on submit) by threading it as `own`/`unique`
([RFC-0026](0026-unique-qualifier.md)/[RFC-0033](0033-place-based-uniqueness.md)).

### 5. Component privacy

A component has a **private** model and private internal `msg`, and emits only
**public output** messages; parent and siblings see only those outputs (module
privacy + the token discipline). A sibling therefore cannot intercept another
component's password because it: (a) has no DOM authority; (b) cannot register
global/capturing listeners (the rune has no such power at all); (c) cannot receive
the owner's private `msg`; (d) cannot construct or unwrap `SecretInput`/`SecretRef`
(sealed, foreign module); and (e) secrets never enter shared app state.

### 6. Untrusted code stays a compartment

Foreign/untrusted renderers keep the existing `Compartment` VNode
([RFC-0015](0015-secure-web-by-construction.md)): a sandboxed iframe, opaque origin,
a JSON grant in and narrow typed events out — distinct from trusted in-rune
components, which are linked into the app and capability-pure unless passed tokens.

### 7. Host-shell contract (defense in depth)

Tokens gate *construction*; the shell still **validates and performs**. On each
`Cmd`, the shell checks the token's `scope`/`methods`/`prefix`/`name` against its
own policy before acting, owns secret custody and one-shot semantics, and marshals
DOM events back as `msg` values. The token is the static, reviewable layer; the
shell is the dynamic enforcement layer. The shell protocol (what each token means;
how fetch/nav/timer/port/secret are authorized) is documented in `spec/` as part of
implementation.

### 8. Effect footprint (Coven auditability)

Because these are bare grantable caps, the effect authority a rune actually uses
surfaces on RFC-0038's granted-user-cap axis: a package's Glamour effect footprint
(`glamour.UiFetch[POST /api/coven/promote]`, `glamour.SecretInput[login.password]`,
`glamour.CredentialPort[promote]`) is reportable by `witchy caps`, and Coven's
block-on-widening gate covers a dependency update that broadens it — the UI mirror
of the package-manager footprint gate.

## Alternatives

- **Status quo — broad public `Cmd` constructors + host-only runtime policy.**
  Rejected: unauthorized effects are representable and only dynamically caught;
  authority is not visible in types or footprints.
- **First-class browser capabilities in Witchy** (`Dom`, `UiNet`, `WebAuthn`).
  Rejected (see [RFC-0038](0038-grantable-user-capabilities.md) Alternatives):
  breaks the pure-compute rune, bloats the TCB, couples the language to platform
  churn. Glamour-provided bare tokens keep browser authority in the shell.
- **Opaque public ADTs / private `Cmd` constructors** as the enabler. Not needed:
  token-gated *public* constructors already make unauthorized effects unusable, so
  RFC-0038 (root-grantable bare tokens) is the smaller, sufficient language change.
- **Raw secret strings with host-side scrubbing.** Rejected: the secret transits
  app state; that is the interception surface we are removing.

## Drawbacks

- Larger Glamour API surface: every sensitive effect needs a token-gated constructor
  plus a smart constructor.
- Narrowing plumbing (`UiRoot` → child tokens) is more ceremony than React's ambient
  hooks/context — deliberate, but a real ergonomic cost.
- Defense in depth means the host shell still validates; tokens do not *remove* host
  policy, so there is intentional double-checking.
- Depends on RFC-0038 landing first.

## Definition of done

- Glamour defines the sealed bare caps; `witchy caps` on the rune shows an **empty
  host footprint**.
- A component without `UiFetch` cannot construct `Cmd.Http` (no smart constructor
  path; direct construction requires the token).
- A differential test: a program where component B attempts to read component A's
  password fails to type-check (no `SecretInput`/`SecretRef` access).
- Both backends + parity: effect *descriptions* are inert data, so the rune stays
  pure and the interpreter and compiled WASM agree.
- coven-web migrated to token-gated effects as the proof product (follow-up; not
  gating this RFC).

## Prior art

- [RFC-0007](0007-witchy-wasm-browser-target.md) (pure-compute rune + host shell),
  [RFC-0008](0008-frontend-framework-rune.md) (MVU-over-VNode),
  [RFC-0015](0015-secure-web-by-construction.md) (compartments),
  [RFC-0002](0002-user-definable-capabilities.md)/[RFC-0038](0038-grantable-user-capabilities.md)
  (sealed + grantable caps).
- Elm's TEA (pure view, typed messages, commands interpreted by a runtime);
  object-capability UI (authority as designated, unforgeable tokens rather than
  ambient DOM/network access).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->

## Change notes

- **2026-07-02 — implemented and frozen.** Glamour declares the sealed bare UI caps
  (`UiRoot` grantable; `UiFetch`/`UiRoute`/`UiTimer`/`CredentialPort`/`SecretInput`/
  `SecretRef` narrowed inside the framework) and gates every sensitive effect on the
  matching token: `Http(UiFetch,…)`, `Nav(UiRoute,…)`, `After(UiTimer,…)`,
  `Port(CredentialPort,…)` (the port name rides the token), and the secret path
  `SecretField(SecretInput,…)` / `SubmitSecret(SecretRef, CredentialPort,…)`. Tokens gate
  construction and are never serialized (the host shell re-validates). Secrets stay in host
  custody: `secret_input` keeps the bytes host-side, the rune holds only an opaque
  `SecretRef`, and `submit_secret` sends the value via a host port. coven-web migrated to
  token-gated effects as the proof product (its six tokens bundled into one `Caps` record).
  DoD met: empty host footprint; a fetch without `UiFetch` and a port without
  `CredentialPort` fail to compile; a sibling that tries to unwrap a `SecretRef` fails to
  compile; the secret wire is byte-identical on both backends. The behavior now lives in
  [`spec/capabilities.md`](../spec/capabilities.md) (§ Framework effect authority) + the code
  (`projects/glamour/src/glamour.witchy`, `web/witchy-runtime/glamour-dom.mjs`).
