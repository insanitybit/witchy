---
rfc: 0038
title: Grantable user-defined capabilities at root entrypoints
status: implemented
created: 2026-07-01
implemented: 2026-07-01 (parse + bareness + main-acceptance + [user_caps] grant + both-backend minting + footprint axis)
predecessors:
  - "0002 (user-definable capabilities — the sealed-brand machinery this builds on)"
  - "0011 (capability refinement — carried state, library-defined tiers)"
  - "0013 (capability grant documents — the launch-grant mechanism this extends)"
tracking:
---

# RFC-0038: Grantable user-defined capabilities at root entrypoints

> **2026-07-01 — implemented.** `grantable capability X:` ships: the surface (a
> contextual-ident prefix, `TypeDef.grantable`), the **bareness rule**
> (`check_grantable_caps` in typeck rejects any transitive host taint),
> `main`-acceptance, the `[user_caps]` grant-document section, and **root minting on
> both backends** — the interpreter builds a `Value::Ctor` from the grant fields;
> the compiled backend stages each policy field host-side (`user_cap_field_len` +
> `fill_pending`) and wraps them in a record via `mk{N}` (each field a separately
> `rc_alloc`'d String — never co-allocated, to keep every field a genuine
> object-base and avoid the SEC-037 dup/free class). Grantable caps surface on a
> `user_caps` footprint axis (`witchy caps`, `compiler.footprint` JSON) and count as
> a widening in the diff. Behavior lives in `spec/capabilities.md` and the code; a
> differential test proves both backends mint identically. RFC-0039 builds on this.
>
> Provisional syntax below is a design record. Code blocks are intentionally **not**
> tagged `witchy` so the doc-examples test does not compile partial snippets.

## Summary

Today authority enters a program in exactly one place — the typed parameters of
`main` — but the *set* of admissible root types is closed: `check_main_signature`
(`crates/witchy-types/src/typeck.rs`) accepts only the built-in host capabilities
(`Console`, `Clock`, `Rand`, `Env`, `Dir`, `File`, `Net`, `Exec`, `Secret`,
`SecretStore`) plus `List(String)`, and grant documents ([RFC-0013](0013-capability-grant-documents.md))
mint only `Dir`/`File`/`Net`/`Secret`/`SecretStore`. This RFC lets a **library**
declare a sealed capability as `grantable` so it can be a root parameter of `main`
(and of other root entrypoints) and be minted by the host from a new `[user_caps]`
section of a grant document. The one load-bearing safety rule: a root-grantable
user capability must be **bare** — carrying *zero* transitive built-in host
authority — so it cannot become a friendly-named disguise for `Net`/`Dir`/`Secret`,
now or after a dependency bump.

## Motivation

Witchy's root-authority vocabulary is hard-coded. Any *new* authority domain — a
UI framework's effect permissions ([RFC-0039](0039-glamour-capability-safe-effects.md)),
a plugin host, a database-pool policy, a workflow-engine grant — has only two bad
options today:

1. **Become a built-in host capability.** This bloats the trusted computing base,
   couples the *language* to every domain's authority model, and forces core
   releases for library concerns. It also breaks designs like the pure-compute
   browser rune ([RFC-0007](0007-witchy-wasm-browser-target.md)), where the WASM
   module is supposed to hold *no* capability.
2. **Smuggle authority through strings + ambient host policy.** The app passes
   stringly-typed requests to a host shell that decides what to honor. The
   authority boundary then lives in runtime policy, not in reviewable types — the
   opposite of Witchy's thesis that *authority is a value you can see in a
   signature and diff in a footprint*.

The language already has the two halves this needs. [RFC-0002](0002-user-definable-capabilities.md)
gives sealed, unforgeable user capabilities (a link-time sealing pass in
`crates/witchy-syntax/src/linker.rs` makes a sealed type constructible/destructurable
*only* in its declaring module). [RFC-0013](0013-capability-grant-documents.md)
gives a reviewable, cross-checked launch grant (`GrantDoc` in
`crates/witchy-caps/src/grants.rs`, bound by name into `main`, diffed against the
computed footprint). The **only** missing piece is admitting a *bare* sealed user
capability at the root and teaching the grant document to mint it.

**Why "bare" is the whole game.** If a user capability could wrap `Net`, then
granting it to `main` would be an *invisible* `Net` grant, and a later version that
adds a `net:` field would silently widen root authority with no change to the
`main` signature. The capability analyzer already computes transitive host-cap
taint to a fixpoint (`type_caps`/`caps_in` in
`crates/witchy-caps/src/capabilities.rs`, which deliberately "sees through" a
wrapper to stay sound). We reuse exactly that to *require* the taint be empty for
anything grantable at the root.

## Design

### 1. `grantable` capability declarations

A sealed user capability may be marked grantable. Grantability is **opt-in**: an
ordinary `capability` is not root-grantable, because some sealed capabilities are
meant to be minted only by a library function after validation (e.g. an
`AuthenticatedUser` you only get by checking a token, never from a launch file).

```
grantable capability UiRoot:
    policy: String
    app_id: String
```

An ordinary (non-grantable) sealed capability is unchanged and still cannot appear
at `main`.

### 2. The bareness rule (the safety core)

A `grantable` capability is well-formed **iff its transitive built-in
host-capability taint is empty**. Concretely it must not, directly or through any
field or nested user capability:

- carry any of `Console`/`Clock`/`Rand`/`Env`/`Dir`/`File`/`Net`/`Exec`/`Secret`/`SecretStore`; nor
- be a `from <HostCap>` refinement of a built-in capability.

This is checked at type-check time by reusing `type_caps` (the same fixpoint the
footprint analyzer runs). A grantable capability that *gains* host taint — a v2
that adds `net: Net[Connect]`, or nests a brand that wraps `Dir` — fails to
compile, at the **declaration site**, before any footprint diff is consulted:

```
error: `Db` is declared `grantable` but carries host capability `Net`
       (via field `pool`); root-grantable capabilities must be bare.
       Construct it inside the program from an explicit `Net` root instead.
```

Bare-but-composed is fine: a grantable `UiRoot` may contain other *bare* user
capabilities (`UiFetch`, `SecretInput`) — the rule is "no transitive host taint,"
not "no capability-typed fields."

### 3. `main` (and other root entrypoints) accept bare grantable caps

`check_main_signature` accepts, in addition to built-in host caps and
`List(String)`, any type naming a **bare, grantable, sealed** capability:

```
import glamour

fn main(console: Console, ui: glamour.UiRoot):
    glamour.run(ui, init, view, update)
```

The same admissibility rule applies to any future non-`main` root entrypoint (an
exported app-root/`step` ABI a framework defines); this RFC specifies the rule,
not additional entrypoints.

### 4. Grant-document extension: `[user_caps]`

`GrantDoc` gains a `[user_caps]` section. Each entry is keyed by the **`main`
parameter name** it binds (mirroring how `[files].config` binds the `config`
parameter), and carries the capability type plus the ordinary policy fields the
host uses to construct the sealed value:

```
[user_caps.ui]
type = "glamour.UiRoot"
policy = "coven-web"
app_id = "coven-web"
```

The grant cross-check (RFC-0013's over/under-grant diff) extends to `[user_caps]`:
a program that binds `ui: glamour.UiRoot` with no matching `[user_caps]` entry is
an **under-grant** (fatal, as today); a `[user_caps]` entry the program never
receives is an **over-grant** (warned). The approval diff (`witchy sandbox
--grants`, `--accept-grants`) lists each granted user cap and its policy fields
alongside the existing `dir`/`file`/`net`/`secret` bindings.

### 5. How the sealed value is minted at the root

The host is *already* the trusted minter of root authority (it mints `Dir`/`File`/…
from the grant today). Two modes, in preference order:

- **(b) Library grant constructor (recommended, general).** The declaring module
  exports a grant function the host calls with the parsed grant fields:

  ```
  grantable capability UiRoot:
      policy: String
      app_id: String

  pub grant fn from_grant(g: GrantFields) -> Result(UiRoot, GrantError):
      # validation, defaulting, schema evolution live here
      Ok(UiRoot(g.get("policy"), g.get_or("app_id", "")))
  ```

  This keeps the RFC-0002 invariant intact — construction still happens **inside
  the declaring module** — and gives the library a place to validate and evolve
  its grant schema.

- **(a) Direct field mapping (sugar).** For a pure-policy capability the host may
  build the sealed record straight from the grant fields. This is the one place
  the runtime constructs a sealed value it does not "declare" — acceptable only at
  the trusted root boundary, exactly as it already mints `Dir`. Offered as sugar
  when no validation is needed; mode (b) is the general form.

### 6. Auditability — the parallel "granted user caps" footprint axis

A bare grantable cap has an **empty host footprint**, so it will not show up in the
existing host-capability footprint — which is correct (it is not `Net`/`Dir`). But
Coven and `caps-diff` still need to *see* it to gate dependency widening. The
analyzer already maintains "a parallel set to the runtime host caps, tracked on a
separate axis" (`capabilities.rs`); we surface grantable user-cap authority there:

- a program's footprint gains a **granted user caps** section listing each bare
  grantable capability a root entrypoint receives (`glamour.UiRoot`);
- `witchy caps` prints it; `caps-diff` treats a newly-required grantable cap (or a
  changed grant schema) as a widening, so Coven's block-on-widening gate covers it;
- each entry records the **declaring package identity/version**, because granting
  `glamour.UiRoot` puts glamour in the *policy* TCB for what that token means and
  how it narrows — a dependency bump that changes that meaning must be visible.

The finer-grained *effect* authority a library layers on top of its root token
(e.g. "this rune can request `UiFetch[POST /promote]`") is library-specific and is
specified in [RFC-0039](0039-glamour-capability-safe-effects.md); this RFC provides
the axis it reports on.

### 7. What stays the same

Everywhere except the root, RFC-0002 sealing is unchanged: no module but the
declarer may construct or destructure a sealed capability. "Never mint from
nothing" still holds — only the *host*, at the root, from an explicit, reviewed,
approved grant document, produces a root capability value.

## Alternatives

- **First-class browser/UI host capabilities** (`Dom`, `UiNet`, `WebAuthn` as root
  built-ins). Rejected: bloats the TCB, couples the language to every platform
  domain, and breaks RFC-0007's pure-compute rune (the rune would *hold* browser
  authority). The library-defined route keeps core small and keeps real platform
  authority in the host shell.
- **Admit *any* user type at `main`.** Rejected: a plain record is forgeable and
  can smuggle host caps; this loses both sealing and "never mint from nothing."
  Only *sealed* + *bare* + *explicitly grantable* qualifies.
- **Allow grantable caps that wrap host caps** (rely on the footprint diff to catch
  laundering). Rejected: it makes root authority depend on a downstream review
  noticing a widened footprint, rather than making the disguise *unrepresentable*.
  The bareness rule fails such a declaration at compile time.
- **A separate "launch profile" system.** Rejected/folded: RFC-0013 grant documents
  *are* the launch-profile mechanism (`--grants`, `grants-check`, approval diff);
  extend them with `[user_caps]` rather than inventing a parallel system. Named
  profiles, if wanted, are ergonomic sugar over grant documents.
- **Do nothing.** Keeps every new authority domain smuggling through strings +
  host policy, which is unreviewable — the failure this RFC exists to prevent.

## Drawbacks

- The host becomes a trusted minter of *library-defined* caps at the root. Mode (b)
  (library grant constructor) narrows this by keeping construction in the declaring
  module; mode (a) widens it (host builds the sealed record) and is why (b) is the
  default.
- Coven must learn the new footprint axis; until it does, grantable user-cap
  widening is invisible to the host-cap gate.
- A grantable cap places its declaring package in the *policy* TCB — a real
  supply-chain surface that the provenance-annotated footprint must surface.
- `grant fn` is new surface: a host-callable entrypoint distinct from `main`.

## Definition of done

- `check_main_signature` accepts a bare grantable cap and **rejects** a grantable
  cap with any transitive host taint (unit tests on typeck, including the nested /
  `from HostCap` cases).
- `[user_caps]` parses, cross-checks (over/under-grant), and binds by name at
  `witchy sandbox --grants`; the approval diff lists granted user caps.
- A `grantable` cap that wraps `Net` fails at compile with a bareness error.
- The footprint reports the granted-user-cap axis with declaring-package
  provenance; `caps-diff` flags a newly-required grantable cap as a widening.
- Both backends + parity: a program receiving and using a bare grantable cap runs
  identically on the interpreter and the compiled WASM tier.

## Prior art

- [RFC-0002](0002-user-definable-capabilities.md) (sealed brands, link-time sealing),
  [RFC-0011](0011-capability-refinement.md) (carried state / tiers),
  [RFC-0013](0013-capability-grant-documents.md) (grant documents + cross-check).
- Object-capability discipline (POLA): authority is a designatable, unforgeable,
  delegatable value; the root is the sole ambient source.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
