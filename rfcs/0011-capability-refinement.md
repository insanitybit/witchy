---
rfc: 0011
title: Capability refinement — carried state, library-defined methods, two tiers
status: partially-implemented
created: 2026-06-23
implemented: 2026-06-24 (Net tier only)
tracking: commits 4bc6d35, 3b84546, b612d25, 246b71a
---

# RFC-0011: Capability refinement — carried state, library-defined methods, two tiers

> Code blocks here are intentionally **not** tagged `witchy` so the doc-examples
> test does not try to compile partial snippets.

> **Status: partially implemented** (Net tier 2026-06-24; carried-state record +
> `dir.subtree` 2026-06-25). Shipped: the **`Net` host-primitive tier** —
> refinement-as-methods (`net.only`/`net.deny`), a typed `NetPolicy` from
> `std/confine` (`tcp`/`any_port`/`cidr`/`cidr_any`, `union`), `current ∩ (∪ allows)
> \ (∪ denies)` enforced host-side; **`dir.subtree(path)`**, the `Dir` host-primitive
> *method* form of `subdir`; and the **RFC-0002 carried-state *record* extension** —
> a sealed `capability X:` may now be a record carrying ≥1 host-cap field plus policy
> fields (so library caps like `Postgres` are expressible), footprint-transparent
> (audits as the union of its cap fields) with private, opaque fields (`match`-only,
> no `.field`/`update`). What did **not** ship yet: `Dir`/`File` *entry* policies
> (`dir.only(kind/ext)`) and retiring `restrict`/`subdir`. Two prose points below are
> also superseded — see *Implementation notes*: `restrict` was **not** retired (it
> survives as the string form), and there is **no `tls()` policy builder / `tls:` in a
> policy** (RFC-0009 as implemented makes `tls:` a connect-time choice on the dialed
> address, not a policy field; `NetPolicy` patterns are scheme-agnostic `host:port`).

## Summary

There is no universal "restriction policy" operation. **Every capability carries its
own refinement state and exposes refinement as ordinary methods that return a
narrower capability of the same sealed type.** This generalizes the one thing witchy
already does — RFC-0003 makes a `Net` *value* carry its own reachable-address set,
enforced at `connect` — to all capabilities: a `Net` carries an address-set, a
library's `Postgres` carries a table-filter, a `User` carries roles. The original
universal `restrict(cap, policy)` builtin is replaced by per-capability refinement
*methods*: `Net` exposes `net.only(...)` / `net.deny(...)` over a typed `NetPolicy`,
`Dir`/`File` expose their own (proposed), and a library that defines a capability
defines its own (`pg.table(...)`). (As shipped, the string `restrict(net,
"host:port")` survives as the config/serialization form — see *Implementation
notes* — and is not retired.)

Refinement lands in **two enforcement tiers**, and the distinction is the heart of
this RFC: a **host-primitive** capability (`Net`/`Dir`/`File`) enforces its carried
state at the syscall (a *hard* guarantee, visible to `witchy caps`); a **library**
capability enforces its carried state inside its own operations (a *softer* guarantee —
the library's correctness — but unforgeable, because the type is sealed, and still
bounded + audited by the host authority underneath). Supporting library-side carried
state requires one extension to RFC-0002: a sealed capability must be able to carry
**policy fields beyond the single underlying capability it wraps**.

## Motivation

The shipped `restrict(net, "…")` has two problems, and the second is the deep one:

1. **It's a per-capability string mini-DSL** — a hidden grammar, parsed at runtime,
   different for every capability, with runtime-only errors. (This was RFC-0011's
   original target.)
2. **It pretends refinement is universal, and it isn't.** A `Postgres` confined to one
   *table*, or a `User` confined to a *role*, has nothing to do with addresses or
   subpaths. Forcing every refinement through one `restrict(cap, policy)` either fails
   (you cannot express "table = users" in an address grammar) or forces `policy` to
   grow into an arbitrarily-extensible, per-domain language — which is just library
   code with extra indirection and no type safety. The library that *defines* a
   capability is the only thing that knows what refining it means; refinement must be
   library-authored, not a builtin.

What we actually want is already latent in the codebase. RFC-0003 didn't add a global
"network policy engine"; it made the `Net` **value** carry its address-set as state,
checked at the action. That pattern — *state on the value, enforced where the
capability acts* — is the general mechanism, and it works for any capability.

## Design

### The shape: refinement is a method that returns a ≤ capability

```
cap.refine(args…) -> cap        # same sealed type; MONOTONE — narrows only, never widens
```

Each capability owns its refinement vocabulary, as methods:

```
let gh   = net.only(tcp("github.com", 443))      # Net's host-primitive refinement (scheme-agnostic host:port)
let pg2  = pg.table("users")                     # Postgres's library refinement
let ro   = user.as(Role.Reader)                  # a User library's refinement
```

A method (not a free function) because refinement *is* the capability narrowing
itself: it reads naturally, it's discoverable/completable, and each capability gets its
own namespace instead of a single overloaded `restrict`.

### Capabilities carry refinement state (generalizing RFC-0003)

A capability value carries refinement state beyond its bare identity, and a refinement
method returns a **new value with narrower state**:

- `Net` carries an **address-set** (RFC-0003). `net.only(…)` intersects it.
- `Postgres` carries a **table-filter** (and a confined `Net`). `pg.table("users")`
  narrows the filter.
- `Dir` carries a **subtree + entry policy**. `dir.subtree("uploads")` /
  `dir.only(kind(File))` narrow it.

### Two enforcement tiers (the crux)

The difference between `Net` and `Postgres` is **where the carried state is checked**:

| Tier | Carried state enforced… | Guarantee | In `witchy caps`? |
|---|---|---|---|
| **Host-primitive** (`Net`, `Dir`, `File`) | at the syscall, by the runtime | **hard** — the kernel will not connect/open elsewhere | yes |
| **Library** (`Postgres.table`, `User.as`) | in the library's *own* operations | **softer** — the library refuses; correctness is its job | no — audits as the underlying cap |

The network cannot enforce "table = users"; the *postgres library* does, by refusing
queries outside the filter in `query`. Library refinement is therefore a *soft*
guarantee (it depends on the library being correct) — but it is **unbypassable by
forgery** (only the sealing module can mint or refine the type, RFC-0002), and the
**underlying host authority stays bounded and audited** (the `Postgres` still audits as
`Net[Connect, Tcp]` to one host, no matter what the table filter says). That is the
honest, useful trade: hard confinement at the host boundary, sealed-but-soft policy
above it.

### The host-primitive vocabulary (Net / Dir / File methods)

The hard tier's methods take **typed policy values**, not strings. A policy value is a
conjunction over dimensions; **multiple arguments are a union** (a set of allowed
shapes); the method **intersects** the union with the current carried scope; and a
**`deny` policy subtracts** (set difference — still monotone, since the result only
shrinks):

```
net.only(tcp("github.com", 443), tcp("redis.internal", 6379))   # union of two endpoints
net.only(tcp(cidr("10.0.0.0/24"), any_port))                    # CIDR, rebinding-proof
net.deny(cidr("10.0.0.0/8"))                                    # everything held EXCEPT this block
dir.subtree("uploads").only(kind(File), ext(".txt"))            # files, not subdirs, .txt only
```

Composition algebra: `effective = current ∩ (∪ allows) \ (∪ denies)`. `deny` is sound
precisely because `restrict` can only ever subtract — a `deny` that could *widen* is the
one thing the type forbids. (This is the typed allow/deny vocabulary the earlier draft
of this RFC proposed, now placed correctly: it is `Net`/`Dir`/`File`'s *own* refinement
methods, the host-primitive tier — not a universal `restrict`.) `restrict(net, "addr")`
and `subdir(dir, "p")` become `net.only(…)` and `dir.subtree("p")`; the string form
survives only as a config serialization (`net.only(net_policy("github.com:443"))`)
for `--net`/manifests/lockfiles.

### The RFC-0002 extension: caps carry policy state, not just one underlying cap

This is the one piece of genuinely new machinery. Today `capability Postgres from Net`
is a one-field brand — *just* the `Net`. A `Postgres` confined to a table must carry a
table-filter **in addition to** its `Net`, and the filter is runtime data (a table
*name*), so it cannot be a facet *type* (no `capability UsersTable` per table) — it must
be **value state**, exactly as `Net` carries its address-set. RFC-0002 is extended so a
sealed capability is a **sealed record** carrying one-or-more underlying capabilities
*plus* policy fields, still **footprint-transparent** to the underlying cap(s):

```
capability Postgres from Net[Connect, Tcp]:        # audits as Net; carries extra state
    tables: TableFilter

pub fn connect(net, host, port) -> Postgres:
    Postgres(net.only(tcp(host, port)), AnyTable)  # confine the Net + start unfiltered
pub fn table(db: Postgres, name: String) -> Postgres:
    db with { tables: Only(name) }                 # ≤, monotone; library-enforced
pub fn query(db: Postgres, sql: String) -> Result(Rows, String):
    require(touches_only(sql, db.tables), 403, "query escapes the table filter")?
    run(db, sql)
```

The footprint analyzer finds the `Net` field and prints `Net[Connect, Tcp] (via
Postgres)`; the `tables` field is ordinary sealed-record state the analyzer ignores
(it is not host authority).

### Monotonicity — proven for host caps, contracted for library caps

A refinement method must return `≤` (narrow only). For the host tier this is structural:
the carried set is intersected (or set-difference'd by `deny`), so the result is always a
subset, and the launch grant remains the hard ceiling (`grant ∩ every refinement`). For
the library tier the type system cannot prove "`table=users` ≤ any-table" in general, so
monotonicity is a **library contract** — but sealing makes the contract *local and
auditable*: only the postgres module can refine a `Postgres`, so the proof obligation
lives in one reviewable place, not at every call site.

## Implementation notes (the Net tier, as built 2026-06-24)

The host-primitive tier shipped for `Net`; the inline examples above that use a
`tls(...)` builder or pass `tls:` inside a policy are **superseded** (RFC-0009, as
implemented, makes `tls:` a connect-time choice on the *dialed address*, so policies
are scheme-agnostic `host:port`). The as-built surface:

- **`NetPolicy` is a typed value** — `type NetPolicy: pattern: String` in
  `std/confine`, built by `confine.tcp(host, port)` / `any_port(host)` /
  `cidr(block, port)` / `cidr_any(block)`. `confine.union(a, b)` joins two policies
  into one multi-endpoint policy (the patterns are newline-joined inside the field).
- **`net.only(p)` / `net.deny(p)` take a `NetPolicy`**, not a string. `only` narrows
  the carried allowlist to the policy's pattern(s) (each must already be admitted);
  `deny` subtracts them (`!`-prefixed entries). `restrict(net, "host:port")` remains
  the **string** form for config/serialization (`--net`, manifests, lockfiles) and is
  *not* retired.
- **How `only`/`deny` accept a typed record without a codegen rewrite:** typeck
  unifies their argument with `NetPolicy`; the interpreter destructures the
  `NetPolicy` record; codegen lowers `args[1].pattern` (a field access) before the
  `net_restrict` / `net_deny` host op. So runtime enforcement (`net_allows`,
  RFC-0003) is unchanged and both backends agree. Method + chained syntax work
  (`net.deny(confine.cidr_any(...)).only(confine.tcp(...))`).
- **`dir.subtree(path)` (2026-06-25)** — the `Dir` host-primitive *method* form of
  `subdir`: same subtree narrowing, lowered to the same `dir_subdir` host op, with the
  same `..`/absolute/symlink confinement. Chains (`dir.subtree("a").subtree("b")`).
- **Carried-state record caps (2026-06-25)** — the RFC-0002 extension shipped. A
  sealed `capability X:` may be a **record** carrying ≥1 host-cap field plus ordinary
  policy fields:

  ```
  capability Postgres:
      net: Net[Connect, Tcp]
      table: String
  ```

  The footprint analyzer's `taint_map` already summed caps over all record fields, so
  this audits as exactly `Net` for free; the link-time seal is field-count-agnostic,
  so construction/`match` stay confined to the home module. The one addition needed
  for soundness: a sealed cap's fields are **opaque** — `.field` access and `update`
  are rejected in typeck, so the only way to reach a carried cap is `match` (which the
  seal confines to the home module). Otherwise an alias (`let raw = pg.net`) would leak
  the underlying authority past the policy. Worked example:
  `examples/carried_state`. Refinement methods (`pg.use_table(...)`) are ordinary
  module functions that rebuild the record with a narrower field — no special
  machinery; the soft-tier check lives in the library's own operations.
- **`subdir` retired (2026-06-25):** the free-function `subdir(dir, p)` is removed;
  `dir.subtree(p)` (method) and `subtree(dir, p)` (free) are the only spellings (the
  underlying `dir_subdir` host op keeps its historical name). Every call site —
  examples, the self-hosted `pm`/`coven` projects, `std/fs`, tests, docs — migrated.
  `restrict` was **not** retired: it survives as the `--net`/config string form (the
  method is `net.only`), so the two coexist by design.
- **Deferred (by decision, 2026-06-25):** `Dir`/`File` *entry* policies
  (`dir.only(kind(File))`, an entry-kind/extension filter carried in the host Dir
  handle). This is the one remaining piece, and it is deliberately deferred: it
  needs an invasive carried-state change to the `Dir` value in both backends plus
  enforcement at every dir op, for a niche confinement — and the Net tier
  (`net.only`/`deny`), `dir.subtree`, and carried-state record capabilities already
  deliver the bulk of the refinement model. Revisit if a concrete, recurring need
  appears.

## Alternatives

- **A universal `restrict(cap, policy)` builtin** — the shipped shape. Rejected: cannot
  express domain refinement (tables, roles) without `policy` becoming an
  arbitrarily-extensible per-domain language, which is library code without the type
  safety. It also mis-teaches refinement as a host operation when most of it is
  application policy.
- **Refinement as facet *types* only** (RFC-0002 facets). Sound for static, finite
  refinements (`Read`/`Write`), but a runtime value (a table *name*, a CIDR) cannot be a
  type-per-value. Carried value state is required; facets remain the right tool for the
  finite/static cases.
- **Per-capability string DSL** (`restrict(net, "tls:…")`, `subdir`). The thing this
  retires; kept only as a config serialization.
- **Do nothing** — leaves `restrict` a mis-framed universal builtin and blocks
  domain-specific refinement (the `Postgres`/`User` cases) entirely.

## Drawbacks

- **Two tiers to teach.** Library refinements are *not* host-enforced; a reader must
  know `pg.table(...)` is the library's promise, not the kernel's. Mitigated by `caps`
  showing exactly the hard tier and sealing making the soft tier unforgeable.
- **New RFC-0002 machinery.** Sealed *records* that carry underlying caps + policy fields
  and stay footprint-transparent are more than the current one-field brand.
- **Library monotonicity is a contract, not a proof.** The type checker can't verify a
  library refinement only narrows; sealing localizes (not eliminates) the risk.
- **Migration.** Every `restrict`/`subdir` call site moves to a method; `fmt`-assisted,
  but real churn across RFC-0003/0009 examples and the capabilities book chapter.

## Prior art

- RFC-0003 (network address scoping) — "scope is carried by the `Net` value," the seed
  this generalizes; its allowlist/CIDR/rebinding rules become `Net`'s method vocabulary.
- RFC-0002 (user-definable capabilities) — sealed brands + footprint transparency,
  extended here to carry policy state beyond one underlying cap.
- `Secret` / `SecretStore` — the existing precedent for a sealed, carried-state
  capability minted only by consuming a root.
- Object-capability attenuation / membranes — refinement as monotone authority reduction
  by value-passing; the host/library split mirrors hard vs. soft membranes.
