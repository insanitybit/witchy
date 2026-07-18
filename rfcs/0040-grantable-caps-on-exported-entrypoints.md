---
rfc: 0040
title: Grantable capabilities on exported root entrypoints (the browser app root ABI)
status: implemented
created: 2026-07-01
implemented: 2026-07-01 (is_string_export + wrapper minting + typeck guard + JS-shell staging + browser value round-trip)
predecessors:
  - "0038 (grantable user capabilities — the minting machinery this generalizes)"
  - "0039 (Glamour capability-safe effects — the consumer this unblocks)"
  - "0007 (witchy-WASM browser target — the pure-compute rune this extends)"
  - "0008 (frontend framework rune — glamour's export_step MVU driver)"
tracking:
---

# RFC-0040: Grantable capabilities on exported root entrypoints

The shipped cap-gated export ABI is specified in
[`spec/capabilities.md`](../spec/capabilities.md), implemented in
[`crates/witchy-lower/src/codegen/assembly.rs`](../crates/witchy-lower/src/codegen/assembly.rs),
and covered by the browser-host test in [`tests/glamour/dom.rs`](../tests/glamour/dom.rs).

> **2026-07-01 — implemented.** `is_string_export` accepts `[bare grantable cap,
> String]`; the `__export_*` wrapper mints the cap host-side (`mk{N}(build_user_cap_field…)`,
> the RFC-0038 machinery pointed at one more site); a typeck guard
> (`check_export_signatures`) rejects a non-grantable leading export param; and the
> pure browser host (`web/witchy-runtime/witchy-runtime.mjs`) provides
> `user_cap_field_len` from the app's `[user_caps]` grant (`opts.userCaps`). Proven
> end-to-end by a node value round-trip (`user-cap-export.test.mjs`, gate-wired via
> `user_cap_export_mints_uiroot_in_the_browser_host`): the minted `UiRoot`'s policy
> reaches the rune, and a missing grant traps (parity with the wasmtime host).
> Behavior lives in [`spec/capabilities.md`](../spec/capabilities.md) + the code. Unblocks RFC-0039's browser
> token-gating.
>
> Provisional syntax below is a design record. Code blocks are intentionally **not**
> tagged `witchy` so the doc-examples test does not compile partial snippets.

## Summary

RFC-0038 mints a bare grantable capability at the CLI root — a parameter of `main`,
staged by the host from a `[user_caps]` grant document. A **browser** glamour app
has no such root: it is driven by the JS host shell calling a pure exported step
function `export_step(input: String) -> String` once per event (RFC-0007/0008), and
its `main` is only a CLI convenience that runs once and returns. So a `UiRoot`
token — needed inside `update`, which runs in those later `export_step` calls —
has no way to enter. This RFC generalizes RFC-0038's minting from `main` to **any
exported root entrypoint**: a grantable cap may be a *leading parameter* of an
`export_*` function, minted by the host from the same `[user_caps]` mechanism, each
call. It is the "app root ABI" RFC-0038 explicitly deferred, and it is what lets
RFC-0039's token-gating reach the browser deployment.

## Motivation

The design fork (surfaced 2026-07-01): witchy's rule is *authority enters at the
root*. In the browser, the root is not `main` — it's the JS-driven `export_step`.
That export is deliberately a pure `String -> String` (the pure-compute-rune thesis:
the rune computes inert descriptions, the capability-holding host shell performs
them). There is **no capability channel** into it, and no `--grants` launch. Two
non-answers were rejected:

- **A bespoke event/async channel** (`main(events: Receiver(DomEvent))`) — witchy's
  executor is a *closed, deterministic, in-VM* scheduler with no host-event
  injection; this would be a whole new host-driven-executor runtime. Too big, and it
  fights RFC-0007.
- **"Just call JS"** — the `Js` capability (RFC-0015) is compartment-spawn authority,
  not general FFI, and it too works by emitting descriptions. There is no synchronous
  JS-FFI, by design.

The resolution keeps the pure-compute model intact and reuses RFC-0038 wholesale:
the token enters the *export*, minted per call. Events keep flowing through the
existing `export_step(model+msg JSON)`; the token just also enters there and threads
into `update` via a closure. The tokens are bare policy data — cheap to re-mint each
call — so there is no persistent-state or forgeability problem.

## Design

### 1. A grantable cap may lead an exported root entrypoint

Today `is_string_export` (`crates/witchy-lower/src/codegen/mod.rs`) accepts exactly
`pub fn export_*(String) -> String`. Extend it to also accept a **leading bare
grantable capability parameter**:

```
pub fn export_step(ui: UiRoot, input: String) -> String:
    let fetch = glamour.fetch_scope(ui, "catalog", "GET", "/api/coven/")
    step_with(input, view, update_with(fetch), …)    # token threads into update via a closure
```

Admissibility (checked with the module in scope, since grantability is a `TypeDef`
flag): the export is `pub`, name-prefixed `export_`, returns `String`, and its params
are either `[String]` (today) or `[<bare grantable cap>, String]`. Bareness is
already enforced by RFC-0038's `check_grantable_caps`.

### 2. The export wrapper mints the cap (reusing RFC-0038)

The `__export_<name>(in_ptr, in_len) -> out_ptr` wrapper today calls `f(in_ptr)`.
For a cap-carrying export it prepends the minted record, exactly as the `run` wrapper
does for `main` (`assembly.rs` `main_args`):

```
__export_step(in_ptr, in_len):
    step(  mk{N}(tag, i64(build_user_cap_field(0, 0)), …),   # the UiRoot, host-staged
           in_ptr )
```

`build_user_cap_field` + `mk{N}` + the `user_cap_field_len` host op are the
RFC-0038 machinery already in the tree — this points them at one more emission site.
The host stages the export's grantable-cap policy fields in `VmState.user_cap_fields`
(browser: the JS shell provides them; see §3), fresh per call.

### 3. The browser host stages the policy (JS shell)

The CLI path already stages `[user_caps]` via `run_file_grants` → `Capabilities`.
The browser analog: the glamour mount API (`web/witchy-runtime/glamour-dom.mjs`)
gains a grant — the app's `UiRoot` policy — and provides the `user_cap_field_len`
import from that grant, so each `__export_step` call mints the same `UiRoot`. The
policy is host-held and unforgeable to the rune (the rune only ever receives the
minted record). The footprint's `user_caps` axis already surfaces `UiRoot`, so the
grant is reviewable.

### 4. How RFC-0039 lands on top

With a real `UiRoot` in `export_step`, glamour narrows it (`fetch_scope`, …) and
threads the token into `update` by returning an `update` **closure** that captures
it — no change to `step_with`'s `fn(model, msg) -> (model, Cmd)` contract. RFC-0039's
token-gated `Cmd` variants (`Http(UiFetch, …)`) then make an unauthorized effect
unconstructable, in the browser as well as the CLI.

## Alternatives

- **Host-driven async executor** (`main` drives the loop over an injected event
  channel). More "authority-at-`main`", but requires a new external-event-injection
  runtime capability that breaks the deterministic-in-VM executor and RFC-0007's
  pure-compute rune. Rejected as far larger and model-breaking.
- **Reconstruct the token from the input JSON.** Forgeable (minting from data breaks
  sealing) unless host-signed — complex, and defeats the point.
- **Do nothing; browser stays shell-policy-gated.** No regression, but the static
  capability guarantee never reaches the browser — the place it matters most.

## Drawbacks

- The host mints a grantable cap at one more entrypoint kind (exports, not only
  `main`) — a slightly wider trusted-minting surface, but the same machinery + the
  same bareness guarantee.
- The token is re-minted each `export_step` call (cheap — bare policy strings), a
  small per-event cost.
- glamour apps that want gated effects must take a `UiRoot` on their export and
  narrow it — a real (but mechanical) migration.

## Definition of done

- `is_string_export` accepts `[bare grantable cap, String]`; a non-bare or non-grantable
  leading param is rejected (reusing RFC-0038's bareness check).
- The `__export_*` wrapper mints the cap via `build_user_cap_field`/`mk{N}`; a
  differential/unit test runs an export with the host staging the policy and confirms
  the cap's fields reach the rune.
- The browser mount API accepts a `UiRoot` grant and provides `user_cap_field_len`
  from it; parity where testable.
- RFC-0039's browser token-gating then builds on this (tracked in 0039).

## Prior art

- [RFC-0038](0038-grantable-user-capabilities.md) (the minting machinery — this is a
  one-entrypoint generalization of it), [RFC-0039](0039-glamour-capability-safe-effects.md)
  (the consumer), [RFC-0007](0007-witchy-wasm-browser-target.md)/[0008](0008-frontend-framework-rune.md)
  (the pure-compute rune + `export_step` MVU driver this preserves).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
