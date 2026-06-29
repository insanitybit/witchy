---
rfc: 0013
title: Capability grant documents
status: implemented
created: 2026-06-23
implemented: 2026-06-28 (parser + footprint cross-check + `--grants` launch + `--accept-grants` approval)
tracking:
---

# RFC-0013: Capability grant documents

> **Status: partially implemented** (2026-06-25). Shipped: the grant-document
> format + TOML parser (`[files]`/`[dirs]`/`[net]`/`[secrets]`) and the
> **footprint cross-check** — the feature of this RFC — in `src/grants.rs`. A
> grant's conferred authority (`GrantDoc::cap_set`) is diffed against the program's
> computed footprint (`capabilities::analyze`): an over-request warns, an
> under-grant is fatal, a match is clean; `Net` is compared at presence level (the
> doc expresses addresses, not the footprint's verb rights), and
> `Console`/`Clock`/`Env` are outside the doc (always host-provided). It is exposed
> as **`witchy grants-check <prog.witchy> <grants.toml>`**, which prints the diff
> and exits 2 on an under-grant (so CI/install tooling can gate on it, mirroring
> `caps-diff`). The **launch path** also ships: `witchy sandbox --grants
> app.grants.toml <prog>` mints the whole capability set from the document —
> binding each `Dir`/`File` `main` parameter to the same-named entry
> (`[files].config` → the `config` parameter), `[net]` to one allowlist,
> `[secrets]` to host-resolved named secrets (`from = "env:VAR"`) — runs the
> cross-check at launch (warn on over-grant, ABORT on under-grant), then runs.
> The **approval diff** is now wired too (2026-06-28): `witchy sandbox --grants`
> prints exactly what `main` will receive (capabilities + each `dir`/`file`/`net`/
> `secret` binding) and, on an interactive TTY, prompts `Approve and run? [y/N]`
> before handing authority over; `--accept-grants` pre-approves for non-interactive
> launches (CI, installers), and a non-TTY launch proceeds after printing the diff
> (the cross-check + under-grant abort remains the hard gate). The one intentional
> non-feature: secrets reach the program **by name** through the `SecretStore`, not
> as a bare `Secret` handle minted from the document.
>
> Code blocks here are intentionally **not** tagged `witchy` so the doc-examples
> test does not try to compile partial snippets.

## Summary

When granting authority to a program by CLI flags gets unwieldy — several files, a
couple of directories, a TLS endpoint, a secret — launch it against a **grant
document** instead: a manifest enumerating exactly what the host hands to `main`. A
program may *ship* a requested-grant document alongside its binary, but it is always a
**request the host/user approves, never a self-grant** — the host decides what authority
to hand over (RFC-0003). And because the package manager already *computes* a program's
capability footprint, a grant can be **checked against that footprint**: a request for
authority the code never exercises is flagged, and a footprint that exceeds the grant
fails at launch. This turns "approve this app's permissions" from blind trust into a
diff against what the code actually does.

## Motivation

- **Flags don't scale.** `--file-read config.toml --file-write run.log --dir-write data
  --net github.com:443 --secret gh=…` is unreadable and error-prone, and it grows
  with every capability a real program needs (RFC-0012 makes single files routine).
- **Programs want to declare their needs.** A rune should be able to say "I need to read
  this config, write this log, and reach this host" so a user reviews it *once* — the
  permissions-manifest pattern, without the permissions-manifest failure mode.
- **The footprint already exists.** witchy computes runtime + build footprints (the PM
  publish gate). Cross-checking a *requested grant* against the *computed footprint*
  catches over-requests automatically — the one thing mobile permission prompts never
  had.

## Design

### The document

A grant document enumerates the capabilities bound to each `main` parameter — a data
file (TOML or witchy data), reviewable and diffable:

```toml
# app.grants.toml
[files]
config = { path = "config.toml", rights = ["Read"] }
log    = { path = "run.log",     rights = ["Write"] }

[dirs]
data = { root = "./data", rights = ["Read", "Write"] }

[net]
# endpoints are scheme-agnostic host:port (RFC-0011 policy values); TLS is a
# connect-time choice on the dialed address (RFC-0009), not part of the allowlist
github = ["github.com:443", "api.github.com:443"]

[secrets]
gh = { from = "env:GITHUB_OAUTH" }    # the host resolves it; never inlined here
```

```
witchy run app.witchy --grants app.grants.toml
```

The host reads the document, opens/mints each capability, and passes them to `main`
(by name). This is purely the *grant* side; the program's declared footprint
(`witchy.toml [capabilities]`) is unchanged — they are two faces of one coin (what the
code may use vs. what the host hands in).

### A shipped grant doc is a request, not a grant

A program **may** distribute `app.grants.toml` beside its binary so a user has a
starting point. It is a **request**:

- The host/user must **approve** it; the program cannot grant itself authority. (The
  host always decides — the RFC-0003 invariant. A self-granting document would
  reintroduce exactly the ambient authority the whole model exists to abolish.)
- Approval can be explicit (`--accept-grants app.grants.toml`) or interactive; the
  default is to **show the diff and require confirmation**, never silent acceptance.

### Footprint cross-check (the part that makes approval meaningful)

Because the PM computes the footprint, the host compares the **requested grant** to the
**computed footprint**:

- **Grant ⊋ footprint** (asks for more than the code exercises) → a **warning**: an
  over-request, the classic trojan-permission smell. Surfaced in the approval diff.
- **Footprint ⊋ grant** (the code needs authority the grant withholds) → a **hard error
  at launch** (the program would fail at the missing capability anyway; fail early and
  legibly).
- **Grant == footprint** → clean; nothing to question.

This is the launch-side mirror of the PM's publish-time footprint gate: there, coven
refuses an under-declared manifest; here, the host refuses an under-grant and flags an
over-grant.

### Semi-broad-then-refine

The grant document is the **reviewed ceiling**, and it is fine for it to be *coarser*
than the program's eventual internal confinement. A well-behaved program takes a
semi-broad grant (a `Net` to a host, a `Dir` over a tree) and **refines internally** —
minting sealed library capabilities and narrowing with RFC-0011 methods as it runs. So:

- the **grant** trades a little blast-radius for ergonomics and review-ability (you
  approve a legible ceiling, not a maximally-minimal enumeration);
- the **program** still drives toward least authority by refinement;
- the **footprint** keeps both honest — the grant can't hide what the code reaches.

This is the project's stance made concrete: hand programs semi-broad, *bounded* (never
ambient) authority, and let safe programs drop and refine as they execute.

## Alternatives

- **CLI flags only.** Fine for one or two capabilities; collapses under a real program's
  needs. The grant document subsumes it (flags remain sugar for the small cases).
- **Program self-grants from its shipped doc.** Rejected — ambient authority by the back
  door; the host must decide.
- **No footprint cross-check.** Leaves approval as blind trust and recreates
  permission-fatigue; the cross-check is the feature.
- **A Turing-complete policy/grant language.** Rejected for the same reason as RFC-0011's
  lambda policies — un-auditable, un-diffable. The document is closed, declarative data.

## Drawbacks

- **Approval fatigue** is a real failure mode of permission manifests. Mitigated, not
  eliminated, by keeping documents minimal/legible and by the footprint diff making each
  approval substantive rather than rote.
- **A document format to design and version** (and keep aligned with the `main`
  signature — a mismatch must be a clear launch error).
- **Two surfaces for authority** (flags + document). Justified by scale, but it is two
  ways in; the document is canonical for non-trivial programs.
- **Trust of the resolver** for indirected secrets (`from = "env:…"`) — the host
  resolves them, so the document never carries a secret value, but the resolution path is
  one more thing to get right.

## Prior art

- RFC-0003 (network address scoping) — "the host decides what authority to hand over,"
  the invariant a shipped grant-request must respect.
- `witchy.toml [capabilities]` + the coven publish footprint gate — the declared-footprint
  side this mirrors on the grant side, and the source of the cross-check.
- RFC-0011 (capability refinement) / RFC-0012 (File) — the policy values and `File`/`Dir`
  grants the document enumerates; the refine-after-grant model.
- Mobile/OS permission manifests — prior art *and* cautionary tale; the footprint
  cross-check is the deliberate answer to their blind-approval failure mode.
