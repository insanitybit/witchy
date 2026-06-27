---
rfc: 0014
title: Remove the retain/without capability firewall
status: implemented        # proposed | planned | implemented | rejected | superseded
created: 2026-06-24
implemented: 2026-06-25
superseded-by:
tracking:
---

# RFC-0014: Remove the `retain`/`without` capability firewall

> **Status: implemented** (2026-06-25). The `retain`/`without` keywords, the
> `Block.restrict`/`CapRestrict`/`RestrictMode` AST, the typeck tombstone
> machinery, and the formatter paths are deleted; the `examples/firewall` project
> is removed and the book/spec/README now teach the structural firewall
> (capture-as-DI). Backends were untouched, so parity held — `cargo nextest`
> stayed green (989 passed) and `clippy -D warnings` is clean. The `restrict(net,
> …)` / `net.only`/`deny` network-narrowing builtins were **kept** (a different
> feature that merely shared the `restrict` name).
>
> Code blocks below are intentionally **not** tagged `witchy` so the doc-examples
> test does not try to compile them; the `retain`/`without` examples describe the
> now-deleted syntax.

## Summary

Delete the block-scoped capability firewall — the `retain` and `without`
keywords and their typeck enforcement. They were meant to give a *hard*,
type-level "this region cannot use capability X" guarantee, but (a) the guarantee
is not actually sound (it hides a name, not a value, so an alias escapes it), (b)
nothing in the codebase uses them outside the chapter that teaches them, and (c)
witchy already has a stronger, unforgeable firewall — **not handing over the
capability** (capture-as-DI + function boundaries). The feature carries two
reserved keywords, a slice of type-checker machinery, and a false security claim,
for zero realized value. Remove it; reframe the book around the structural
firewall that already works.

## Motivation

### Nothing uses it

Across the entire tree, `retain`/`without` appear as block statements in **8
places, all inside `book/src/capabilities-narrowing.md`** — the chapter that
documents the feature. Zero uses in `examples/`, `std/`, or `projects/` — not
even `coven-web`, the most security-conscious program we have. A capability
construct whose only consumer is its own tutorial is not earning two keywords and
a type-system extension.

### The real firewall already exists and is unforgeable

witchy's primary least-authority mechanism is structural: **a function or closure
that never receives a capability cannot use it.** Capture-as-DI makes this
un-bypassable — there is no name to reach, no value to alias, nothing to forge.
`retain`/`without` are a strictly weaker echo: they operate *inside a scope that
still holds the capability* and ask the type checker to pretend, for a few lines,
that the name is gone.

Concretely, in `coven-web` the static-asset handler cannot reach the upstream
`Net` — not because of a `without` block, but because the handler closure never
captured that `Net`. The structural firewall already does the job in exactly the
place one would reach for the keyword.

### The "hard guarantee" is not sound

The firewall hides a **name** in a scope, not the **value** behind it. So an
alias taken before (or around) the block escapes it:

```
let n = net                 // alias created outside the firewall's view
without net:
    connect(n, "evil:1")    // `net` is tombstoned, but `n` is not — escape
```

This is the parked "value-taint" gap noted in
[RFC-0002](./0002-user-definable-capabilities.md) and
[RFC-0003](./0003-network-address-scoping.md). It means the feature delivers
**lint-grade assurance dressed as a type-level boundary** — the worst
configuration: full language-feature cost, best-effort guarantee. The book
chapter's claim that code inside a firewall "still cannot touch the network" is,
for the aliasing case, false.

### It gets worse under the roadmap, not better

[RFC-0002](./0002-user-definable-capabilities.md) makes capabilities storable in
struct fields and sendable across `spawn`/channel boundaries;
[RFC-0003](./0003-network-address-scoping.md) adds per-value `Net` scopes. Each
multiplies the ways a capability value can be aliased away from the name a
firewall tombstones. Keeping `retain`/`without` means committing to extend an
already-incomplete soundness proof across all of that surface, forever. Cutting
it now removes that obligation.

## Design

This is a removal. The feature is compile-time only and backend-transparent, so
the change is contained to the front end, the formatter, and docs — **no
interpreter, runtime, or codegen changes, and parity is unaffected.**

### What is removed

- **Lexer** (`src/lexer.rs`): the `Tok::Retain` / `Tok::Without` token variants,
  their `Display` arms, and the `"retain"` / `"without"` keyword mappings.
- **AST** (`src/ast.rs`): the `Block.restrict: Option<CapRestrict>` field, the
  `CapRestrict` struct, and the `RestrictMode` enum. Every `Block { … }`
  constructor drops the `restrict` field.
- **Parser** (`src/parser.rs`): `restrict_block` and the `Tok::Retain` /
  `Tok::Without` dispatch in statement parsing.
- **Type checker** (`src/typeck.rs`): the firewall machinery — the per-frame
  `tombstones` parallel to `scopes`, `is_firewalled`, and the hidden-name lookup
  rules (the "a name an outer firewall dropped is a legitimate shadow, not a
  leak" logic). Ordinary scope/shadowing is unaffected.
- **Formatter** (`src/format.rs`): `restrict_header` and the block-rendering
  paths that emit `retain`/`without` headers.
- **Tests**: `src/format_tests.rs::preserves_capability_firewalls` (delete). Any
  other test that *names* a firewall block is rewritten to the capture-as-DI form
  (see Migration).
- **Reserved words**: `retain` and `without` return to being ordinary
  identifiers.

### What is explicitly NOT removed

- **`restrict(net, addr)` / `net.restrict(...)`** — the network-address narrowing
  *builtin* — is a different feature (see `capability-rights.md` and
  [RFC-0003](./0003-network-address-scoping.md)). The AST field shares the
  `restrict` name with the firewall, which is a naming collision, *not* a shared
  feature. The net-narrowing op and its tests stay.
- **Capability narrowing with `as`** (`dir as Dir[Read]`, `subdir`) — unrelated,
  stays.
- **The footprint analyzer's capability-taint pass** (`src/capabilities.rs`
  `caps_in`/`taint`) — this computes *footprints* through brands; it is not the
  firewall and stays.

### What replaces it

Nothing new is added — the replacement already exists. Least-authority *within* a
function is expressed by extracting the restricted work into a function (or
closure) that does not receive the capability. The guarantee is stronger because
it is structural, not a checker convention:

Before (firewall — best-effort, alias-leakable):

```
fn handle(req: Request, net: Net, dir: Dir, console: Console) -> Response:
    let body = render(req)
    without net:
        audit_log(console, dir, body)   // "promise" not to dial here
    respond(body)
```

After (capture-as-DI — unforgeable):

```
fn audit_log(console: Console, dir: Dir, body: String):   // never receives `net`
    ...                                                    // structurally cannot dial

fn handle(req: Request, net: Net, dir: Dir, console: Console) -> Response:
    let body = render(req)
    audit_log(console, dir, body)       // `net` is simply not in scope inside
    respond(body)
```

`audit_log` cannot reach the network in *any* execution, with *any* aliasing,
because the authority was never passed to it.

### Migration

- **Book**: rewrite `book/src/capabilities-narrowing.md`. Delete the "Block
  firewalls: `retain` and `without`" section. Where it taught block firewalls,
  teach the structural firewall: pass each region only the capabilities it needs;
  the absence of a parameter *is* the boundary. This also corrects the chapter's
  current over-claim about firewall blocks being sealed.
- **Code/tests**: the only in-tree firewall uses are doc examples and one
  formatter test; rewrite or delete them as above. (`fmt` is not a migration
  vehicle here — the old syntax is removed, not reformatted — but since no real
  program uses it, the migration is essentially the book rewrite.)
- **Pre-1.0 / break-don't-deprecate**: no alias, no shim. The keywords are gone in
  one cut; a program that used them gets a parse error pointing at the
  capture-as-DI idiom.

## Alternatives

- **Keep it as-is.** Rejected: carries keywords + typeck machinery + a false
  "hard guarantee," with zero realized use, and the soundness debt grows under
  RFC-0002/0003.
- **Make it sound** (real value-taint / escape analysis so an aliased capability
  is also tombstoned). Rejected for now: this is substantial,
  security-sensitive analysis that must be parity-clean and must track
  capabilities through struct fields, closures, and channels (RFC-0002). That is
  a large investment to make a feature *nobody uses* match a guarantee
  capture-as-DI already provides for free. If a concrete, recurring need ever
  appears, this is a separate RFC.
- **Demote to an opt-in lint** — keep `without c:` as an advisory static check
  that flags *direct naming* of a dropped capability in a region, honest about
  being best-effort (no soundness claim, no type-system tendrils). A reasonable
  middle path, and the right fallback **if** real demand surfaces. But the usage
  data says the demand has not surfaced, and a lint still spends keyword and
  teaching surface. Recommendation: remove now; reintroduce as a lint only on a
  demonstrated, repeated need that capture-as-DI genuinely cannot express.

## Drawbacks

- **Loss of inline-intent ergonomics.** For a large handler that holds several
  capabilities, marking "this stretch must not touch the network" now requires
  extracting a function rather than wrapping a block. That is a small ergonomic
  cost — and the extraction yields a *stronger* guarantee — but it is a real
  change in how that intent is expressed. The usage data suggests this case is
  rare in practice.
- **A documented feature disappears.** The narrowing chapter must be rewritten,
  and anyone who learned `retain`/`without` from it must relearn the idiom. Pre-1.0
  this is the cheapest it will ever be.
- **Closes a door (reopenable).** If a future need genuinely cannot be served by
  passing-discipline, we will have removed the scaffolding and must reintroduce it
  (as a sound feature or a lint) deliberately. Given the feature is unused and
  unsound today, paying that option cost later is preferable to carrying it now.

## Prior art

- [RFC-0002](./0002-user-definable-capabilities.md),
  [RFC-0003](./0003-network-address-scoping.md) — both name the firewall's parked
  value-taint/aliasing gap and would expand its surface; the motivation to cut
  rather than extend comes from there.
- `capability-rights.md`, `secrets-design.md` — the capability-narrowing and
  authority model the structural firewall (capture-as-DI) rests on.
- `book/src/capabilities-authority.md` / `capabilities-optional.md` — the
  capture-as-DI discipline that becomes the chapter's sole firewall story.
- Object-capability discipline (POLA): authority comes from *holding* a
  capability, so the un-bypassable way to deny authority is to *not grant the
  reference* — which is precisely capture-as-DI, and precisely what a name-hiding
  block firewall only approximates.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
