---
rfc: 0002
title: User-definable capabilities
status: implemented
created: 2026-06-21
superseded-by:
tracking: "core shipped 2026-06-21 (capability decl + sealing + facets); rights lattice deferred (facets cover it)"
---

# RFC-0002: User-definable capabilities

> Provisional syntax. Code blocks here are intentionally **not** tagged `witchy`
> so the doc-examples test does not try to compile them.
>
> **2026-06-21 — implemented (core).** `capability X from U` (and `from (A, B)`)
> ships: it desugars to a SEALED one-variant brand (`TypeDef.sealed`), reusing the
> existing branded-capability machinery (runtime value on both backends, and the
> footprint analysis that already *sees through* a brand to its underlying caps —
> `caps` prints `… (refined: X)`). The new enforcement is a **link-time sealing
> pass** (`src/linker.rs`): a sealed type may be CONSTRUCTED or DESTRUCTURED only
> in its declaring module, so the brand is un-forgeable across modules/runes (the
> cross-rune forge that motivated this RFC is now rejected). **Attenuation is via
> facets** (a `capability Y from X` refining another capability), as recommended
> below; the optional **rights lattice** (`X[Read]`) is not implemented — facets
> cover it. Minting is the sealed constructor (`X(u)`), which by construction
> consumes the underlying capability `u`, satisfying "never mint from nothing".

## Summary

Let a *library* define its own capability types by **attenuating and composing
the host's** — e.g. a `redis` rune ships a `Redis` capability built from a `Net`
bounded to the server, a `postgres` rune ships a SQL capability that distinguishes
read from write, a `secrets` rune ships a KMS-shaped key manager. A user
capability is **sealed** (no public constructor — nothing to forge), is **minted
only by consuming host roots** (a library can never conjure authority), and is
**transparent to the footprint analyzer** (`witchy caps` sees through it to the
primitive authority underneath). This turns witchy's fixed capability menu into
an open, composable, auditable authority system, and it resolves the
brand-forging gap (RFC discussion below) **for the cases that need a hard
guarantee — authority — without introducing type privacy or special
constructors.**

This RFC formalizes the "library-defined-capability north star" sketched in
[`secrets-design.md`](./secrets-design.md) and generalizes the implemented
rights model in [`capability-rights.md`](./capability-rights.md).

## Motivation

### The capability menu is closed

Today the capability set is a fixed enum hard-coded in the type system
(`Ty::Console`, `Ty::Dir(DirRights)`, `Ty::Net(NetRights)`, `Ty::Secret`, …). But
real programs talk to real resources — Redis, Postgres, an SMTP server, a vault,
a GPIO pin, a rate-limited HTTP endpoint — each of which *is* a distinct
authority with its own verbs. With the menu closed, a `redis` library can only
take a raw `Net`, which means:

- a function that "uses Redis" audits indistinguishably from one that can dial
  *anywhere* on the network, and
- holding the handle a library hands you is no weaker than holding the raw `Net`.

We want libraries to express *"this is authority to talk to one Redis server, and
nothing else"* as a first-class, narrowable, auditable type.

### The brand-forging gap (why not "just use a struct")

A library *can* wrap a `Net` in a one-field type (`type Redis: Redis(Net)`) — the
"branded capability" pattern in `examples/branded_caps`. But user constructors
are forgeable everywhere, including across a rune boundary (reproduced: a consumer
rune writes `Even(3)` / `ConfigDir(rawDir)` directly, bypassing the library's
smart constructor). So a struct brand records *intent* but enforces *nothing*, and
the book's claim that a branded value "provably came through that gate" is, today,
false.

We considered closing that gap in the type system and rejected the options (see
**Alternatives**): general opaque types / `pub`-on-types reintroduce a visibility
bifurcation we don't want; constructor-override (`__init__`-style) makes
construction fallible and needs a hidden raw builder anyway. The key realization:

> **The only thing that needs an *un-forgeable* abstraction is authority — and
> authority already has an un-forgeable mechanism (capabilities have no
> constructor). So route the hard cases through the capability system, and leave
> plain data to convention.**

witchy already ships exactly one instance of this shape: `Secret` is minted by
`SecretStore` (`secretstore.get(store, name) -> Option(Secret)`), is opaque, and
exposes a tiny operation surface (`crypto.reveal` / `sign`). "User-definable
capabilities" is that pattern, made declarable by any library.

### What we explicitly are *not* trying to fix

Pure-data validation brands — `Email`, `Positive`, `SortedList` — stay
**convention** (the `_`-style soft guarantee). They are not authority; a forged
`Email` hurts only the forger's own program. Drawing the hard/soft line at *"is it
authority?"* is the whole point, and it's what lets data construction stay
non-special and privacy-free.

## Design

### The two primitives this rides on

Per `secrets-design.md`, the entire feature reduces to two general,
non-resource-specific language primitives; **everything else is library code.**

1. **Sealed types** — a type whose constructor is private to its declaring
   module, so that module is the sole, unforgeable minter. This promotes today's
   implicit "branded capability" into a first-class guarantee and is the
   unforgeability anchor.
2. **Capabilities as storable / sendable values** — a capability may live in a
   struct field and cross `spawn` / channel boundaries (servers, pools,
   concurrent clients). This is a real usability gap today and is required for any
   non-trivial capability-bearing library.

### Declaring a capability

```
capability Redis from Net[Connect, Tcp]
```

`capability X from U` declares `X` as a **refinement of** `U` (one underlying
capability, or several — see *Composition*). Consequences, all automatic:

- **No public constructor.** There is no `Redis(...)` literal anywhere. `X` is
  sealed: it can only be minted in `X`'s declaring module.
- **In-module transparency.** Inside the declaring module, a value of type `X` is
  usable wherever its underlying `U` is expected — that is how the library
  implements `X`'s operations (it calls `U`'s operations on it).
- **Out-of-module opacity.** Elsewhere, `X` is opaque: callers see only the
  operations the library exports, and **cannot recover `U`** (unless the library
  chooses to expose that — see *Unwrap*).

### Minting: never from nothing

A capability is minted by **attenuating an existing one you already hold** — the
same shape as `dir as Dir[Read]` or `subdir`. Spelled with `as`, legal only in
the declaring module:

```
// in the redis module
pub fn connect(net: Net[Connect, Tcp], host: String, port: Int) -> Result(Redis, String):
    let probe = dial(net, "${host}:${port}")?
    Ok(net as Redis)            // mint — consumes a real Net; module-only
```

**The non-negotiable soundness invariant** (from `secrets-design.md`): a library
may *attenuate / compose* authority into a new sealed type, but may **never mint
authority from nothing.** A mint must *consume* a host-rooted capability the
caller already passed in. The fixed host capability set stays the trust anchor;
every user capability's authority is transitively rooted in it. (Enforcement: the
`as X` mint type-checks only when the operand is `X`'s declared underlying
capability — there is no nullary mint.)

### Footprint transparency (the load-bearing rule)

A user capability **audits as the primitive authority it wraps.** `witchy caps`
must see *through* `Redis` to `Net[Connect, Tcp]`:

```
$ witchy caps app.witchy
  redis.get   Net[Connect, Tcp]   (via Redis)
  main        Console, Net
  total       Console, Net
```

A library therefore *cannot launder* `Net` authority behind a friendly name —
the footprint is computed from the underlying caps the operations actually
exercise, annotated with the refinement. This generalizes the existing,
tested guarantees (`a_branded_capability_cannot_hide_a_widening`,
`library_cannot_fabricate_a_capability` in `src/*_tests.rs`) from one-field
brands to declared capabilities.

### Attenuation: facets (primary) vs. a rights lattice (optional)

There are two ways to hand out *less* of a user capability. This RFC makes
**facets the primary mechanism** and a **rights lattice an optional extension**,
reconciling this RFC's examples with `secrets-design.md`'s preference.

**Facets — a narrower sealed type exposing fewer operations.** Attenuation is
then just ordinary type-checking: the narrower type structurally lacks the
dangerous method.

```
capability Postgres   from Net[Connect, Tcp]   // full: query + execute
capability PgReader    from Postgres            // facet: only query

// in the postgres module
pub fn reader(db: Postgres) -> PgReader:  db as PgReader   // mint a facet (module-only)
pub fn query(db: PgReader, sql: String) -> Result(Rows, String): run(db, sql)
pub fn execute(db: Postgres, sql: String) -> Result(Int, String): run(db, sql)
```

A consumer handed a `PgReader` has *no* `execute` in scope and cannot reconstruct
a `Postgres` (sealed), so it is provably read-only — enforced by the type checker,
no new machinery beyond sealed types. This is `secrets-design.md`'s "facet
pattern" (`signer = sm.signer_for(...)`).

**Rights lattice — `X[Verb]`, optional sugar** for capabilities that want
verb-precise footprints and `caps-diff` widening detection without a type per
facet, exactly like the built-in `Dir[Read]` / `Net[Connect]`
([`capability-rights.md`](./capability-rights.md)):

```
capability Postgres from Net[Connect, Tcp]:
    rights Read, Write

pub fn query(db: Postgres[Read],  sql: String) -> Result(Rows, String): run(db, sql)
pub fn execute(db: Postgres[Write], sql: String) -> Result(Int, String): run(db, sql)
```

```
$ witchy caps-diff before after        # daily_report: Postgres[Read] -> Postgres[Write]
  daily_report: Postgres[Read] -> Postgres[Write]   WIDENED   (exit 2)
```

The rights lattice is strictly more machinery (user-defined right-sets that
narrowing, `as`, `caps`, `caps-diff`, and the firewall must all understand). The
recommendation is **facets-first** — they need only primitive #1 and reuse
ordinary type-checking — and add the rights lattice only if real libraries find
the per-facet type proliferation painful.

### Composition

A capability may refine more than one underlying capability:

```
capability Mailer from (Net[Connect, Tcp], Secret)   // SMTP socket + DKIM signing key

pub fn smtp(net: Net[Connect, Tcp], signing: Secret, host: String) -> Result(Mailer, String):
    let probe = dial(net, "${host}:587")?
    Ok((net, signing) as Mailer)        // mint from the pair; module-only
```

`Mailer` audits as `Net + Secret`; both underlying authorities show through.

### Unwrap (a per-capability choice)

If a library *never* exposes "give me back the underlying `U`," then holding the
facet is *strictly less* authority than holding `U` — the genuine attenuation that
makes this worth doing. A library *may* choose to expose an unwrap (`as U`,
re-exported), making the capability a soft wrapper instead. **Default and
recommendation: never expose unwrap.** This is the knob that decides whether a
capability is real attenuation or mere documentation.

### Interaction with existing capability machinery

User capabilities are capabilities, so they ride the machinery uniformly:

- **Narrowing** — more authority stands in for less at call boundaries; `as`
  drops to a facet/rights-narrowed handle.
- **Firewall** — `retain c` / `without c` drop a user capability for a region
  exactly like a built-in. (Note: this *adds* surface to the firewall's
  value-taint work; see Drawbacks and the parked firewall finding.)
- **`caps` / `caps-diff` / `audit`** — see through to underlying authority; the
  rights extension participates in widening detection.
- **Comptime** — a `comptime:` block still cannot obtain any capability (no
  parameter list), so it cannot mint one either.

### Type-system changes

- The closed capability enum (`Ty::Dir(DirRights)`, `Ty::Net(NetRights)`,
  `Ty::Secret`, …) gains an open variant, conceptually
  `Ty::UserCap { name, underlying: Vec<Ty>, rights: RightSet }`.
- `DirRights` / `NetRights` (today hardcoded structs) generalize to a
  library-declared **right-set** the narrowing/diff/firewall code is written
  against generically.
- The footprint analyzer's "see through a brand" pass generalizes to "a user
  capability contributes the footprint of its underlying caps, tagged with the
  refinement."
- Sealing: constructing/`as`-minting a `UserCap` is a type error outside its
  declaring module; the smart constructor and operations live inside it.

### Worked examples

See `scratch/cap_model/` for the full programs (Redis, Postgres both ways,
Mailer + firewall, and `Secret` re-read as an instance of this model). The
`Secret` / `SecretStore` example demonstrates the model is already implemented
in one closed form — this RFC opens it to libraries.

## Alternatives

- **Do nothing.** Brands stay convention; the capability menu stays closed; the
  `branded_caps` security claim stays false (or must be softened in docs).
  Rejected: it forgoes a distinctive, high-value feature and leaves a documented
  guarantee untrue.
- **General opaque / abstract types (`pub type`, private fields).** The Rust/OCaml
  route. Rejected for *this* purpose: it reintroduces a type-visibility
  bifurcation we deliberately want to avoid, and it would make *every* type carry
  a privacy axis to solve a problem only authority actually has. (It remains the
  right tool *if* we later decide pure-data brands need hard guarantees — see
  Drawbacks.)
- **Constructor override (`__init__`-style).** Make `T(args)` run user logic.
  Rejected: it makes construction fallible (rippling `Option` to every call site)
  and needs a hidden raw builder anyway (the regress), which is sealed-types by
  another name with worse ergonomics.
- **Implicit rule: "a type with a capability field constructs only in its
  module."** Essentially sealed types, but *automatic and invisible*. Rejected in
  favor of an explicit `capability` declaration: the intent ("this is authority")
  should be stated, audited, and documented, not inferred from a field type.
- **Rights lattice as the *only* attenuation.** Rejected as the default in favor
  of facets (less machinery); kept as an optional extension.

## Drawbacks

- **Sealing is a contained sliver of privacy.** "Mint/unwrap only in the declaring
  module" is, technically, a privacy rule — the one concession. It is confined to
  the single operation where it is intrinsic and principled (the same concession
  already accepted for the host minting `Dir`/`Secret`), but a strict "zero
  privacy anywhere" stance pays this price.
- **Pure-data validation stays convention.** `Email`, `Positive`, `SortedList`
  get no hard guarantee under this model unless a library models them as
  capabilities (a category stretch). This is by design, but it is a real
  limitation: if a future need demands un-forgeable *data*, that's a separate
  decision (general opaque types), not this RFC.
- **Real type-system work.** Opening the capability enum, generalizing the rights
  representation, and threading user caps through narrowing / footprint / firewall
  / `caps-diff` is substantial, security-sensitive surface.
- **More capability types ⇒ more firewall surface.** The parked firewall
  value-taint gap (a dropped cap escaping `retain`/`without` via aliasing) now
  applies to every user capability too; the two efforts interact.
- **Two attenuation mechanisms.** Facets + an optional rights lattice is a choice
  that could fragment library conventions if not guided (recommend facets-first).
- **Backend parity.** Capabilities-as-storable/sendable values and sealed minting
  must behave identically on the interpreter and compiled WASM (the `Secret`
  handle precedent shows this is achievable — host-side table + i32 handle on
  WASM).

## Prior art

- [`secrets-design.md`](./secrets-design.md) — the "library-defined-capability
  north star," the two-primitives framing, the never-mint-from-nothing invariant,
  and the facet pattern this RFC adopts.
- [`capability-rights.md`](./capability-rights.md) — the implemented `Dir[r]` /
  `Net[v,t]` rights model this RFC's optional rights lattice generalizes.
- The shipped `Secret` / `SecretStore` capability (`src/capabilities.rs`) — a
  working, parity-tested instance of the pattern.
- Object-capability discipline (POLA), the E language, and KMS/Secrets-Manager
  service shapes (use-don't-read keys) — the design lineage for
  attenuation-by-facet and authority rooted only in granted caps.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
