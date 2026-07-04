---
rfc: 0057
title: "Capability policy constructors: home refinement vocabulary on the capability, retire std/confine"
status: proposed
created: 2026-07-03
predecessors:
  - "0011 (capability refinement — established refinement-as-methods; this finishes the job for the policy-value half)"
related:
  - "0042 (module namespaces — `Type.fn` associated calls must resolve alongside `module.member`)"
  - "0050 (method-call generalization — defines the dotted-call surface these compose with)"
  - "0056 (keyword arguments — labeled `Net.tcp(cidr: …, port: …)` composes once labels land)"
tracking:
---

# RFC-0057: Capability policy constructors

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not try to compile pre-implementation
> snippets.

## Summary

[RFC-0011](0011-capability-refinement.md) established the thesis that *"every
capability carries its own refinement state and exposes refinement as ordinary
methods that return a narrower capability of the same type"* — and retired the
universal `restrict(cap, policy)` builtin for exactly that reason. The refinement
**verbs** landed on each capability: `net.only(…)` / `net.deny(…)`, `dir.only(…)`,
and a library cap defines its own (`pg.table(…)`).

But the refinement **policy values** did not move. They live in a single shared
module, `std/confine`, which lumps two unrelated capabilities' vocabularies
together: `confine.tcp` / `cidr` / `cidr_any` / `any_port` / `union` / `private`
(a `Net` address policy) **and** `confine.ext` / `files` / `dirs` (a `Dir` entry
policy). So `net.only(confine.tcp(…))` reaches across to a shared grab-bag for a
value that morally belongs to `Net`. `confine` is the residue of the retired
universal `restrict`: the verbs moved onto each capability; the constructors were
parked in one place and never followed.

This RFC finishes RFC-0011. It introduces **type-associated functions** —
`TypeName.func(…)`, declared in an inherent `impl TypeName:` block — and homes each
capability's policy constructors under that capability's own type: `Net.tcp(…)`,
`Net.private()`, `Dir.ext(…)`, `Dir.files()`. `std/confine` is deleted. The
refinement verbs are unchanged: `net.only(…)` / `net.deny(…)` / `dir.only(…)` stay
exactly as they are (a deliberate scope decision — see *Non-goals*).

```
// before — a shared grab-bag that knows about both Net and Dir:
let db   = net.only(confine.tcp("10.0.0.5", 6379))
let safe = net.deny(confine.private())
let logs = dir.only(confine.ext(".log"))

// after — each capability owns its policy vocabulary under its own type:
let db   = net.only(Net.tcp("10.0.0.5", 6379))
let safe = net.deny(Net.private())
let logs = dir.only(Dir.ext(".log"))
```

## Motivation

**The asymmetry is the smell.** RFC-0011's whole point is that there is no
universal restriction operation — each capability owns its refinement. A reader who
has internalized that sees `net.only(…)` and correctly expects `Net` to own the
thing it is narrowed *to*. Instead the policy comes from `confine`, a module whose
only job is to be the place the old universal `restrict` left its arguments. The
name even reads like the retired verb it replaced.

**The grab-bag mixes concerns.** `confine.tcp` (a `host:port` allowlist pattern,
enforced against the resolved IP at `connect`) and `confine.ext` (a filename-suffix
filter, enforced at `read`/`write`/`open`) have nothing in common except that both
are "a policy." Co-locating them means adding a third capability's policy (say a
library `Postgres.table(…)`) has no principled home: it either bloats `confine`
with a dependency on a library type, or it lives on `Postgres` while `Net`/`Dir`
policies live in `confine` — an incoherence.

**A library cap can only own half its refinement today.** RFC-0011 says a library
that defines a capability defines its own refinement, and `pg.table(…)` (a verb)
works. But the *policy value* a verb consumes has no type-owned home — a library
author cannot write `Postgres.table("users")` as a constructor namespaced under
their own capability, because witchy has no type-associated functions. This RFC is
what makes RFC-0011's "a library defines its own" true for the value half too.

**Verified against the shipped binary (2026-07-03):** method calls dispatch on a
receiver *value* — `only`/`deny` are instance methods on a `Net` value
(`crates/witchy-types/src/typeck.rs:1976`). There is **no** `Type.func` form:
`Net.tcp(…)` does not parse as a call today. That is the one gap between the current
design and the end state this RFC describes.

## Design

### 1. Type-associated functions

A new item form: an **inherent impl block** declares functions associated with a
type, called by qualifying with the type name.

```
impl Net:
    // A plaintext TCP endpoint policy: <host>:<port>.
    pub fn tcp(host: String, port: Int) -> NetPolicy:
        NetPolicy(host + ":" + "${port}")

    // The non-public IP ranges (loopback, RFC-1918, link-local incl. the cloud
    // metadata IP, CGNAT, IPv6 equivalents). Matched against the RESOLVED IP.
    pub fn private() -> NetPolicy:
        NetPolicy(/* … the CIDR set … */)
```

Called as `Net.tcp("10.0.0.5", 6379)` and `Net.private()`. An associated function:

- Takes **no receiver** — it is namespaced under the type, not called on a value.
  (`net.tcp(…)` on an *instance* remains a normal method call and is unrelated.)
- Is `pub`/private like any function, exported from its declaring module.
- May return any type — here a `NetPolicy` — and, crucially, needs no capability to
  build a pure value, so a policy constructor's footprint stays empty.

This mirrors Rust's inherent-impl associated functions (`Vec::new`) and is the
minimal feature that lets a *type* own a constructor. It is deliberately small: no
`Self`, no associated types, no trait-associated dispatch in v1 — just "functions
namespaced under a type name."

### 2. `NetPolicy` / `DirPolicy` are owned by their capability

The policy types move out of `std/confine` to sit with the capability whose policy
they are. Under [RFC-0042](0042-module-namespaces.md)'s type→module ownership, the
natural home is the module that owns the capability surface (the prelude for the
host primitives `Net`/`Dir`/`File`). The associated functions that build them live
in that same `impl Net:` / `impl Dir:` block, so a reader finds a capability's
entire refinement vocabulary — verbs *and* policy constructors — in one place.

The **enforcement** is untouched: a `NetPolicy` still wraps the same `host:port`
allowlist pattern string that `witchy_caps::capabilities::net_only` /
`dir_admits` enforce host-side on both backends (RFC-0003 / RFC-0011). This RFC
moves *where the constructor is spelled*, not *what it means* or *how it is checked*.

### 3. Resolution: `Type.fn` vs `module.member`

A dotted name `A.b` now has one more possibility. Resolution order at a call site:

1. If `A` binds a **local value** (a variable of a type with method `b`) → method
   call on the value (today's behavior; `net.only(…)`).
2. Else if `A` names a **type** with an associated function `b` → associated call
   (`Net.tcp(…)`) — new.
3. Else if `A` names a **module** with a `pub` function or type `b` → module member
   (`iter.map`, and RFC-0042's `iter.Step`).

A type and a module may not share a name (already true — a module *is* its file,
types are Title-case; `Net` is a type, never a module), so 2 and 3 never both match.
The lowercase/Title-case convention keeps 1 (values are lowercase) visually distinct
from 2 (types are Title-case) at the call site: `net.only` reads as an instance verb,
`Net.tcp` as a type constructor.

### 4. Library capabilities

`impl` on a user capability lets a library complete RFC-0011's promise:

```
capability Postgres:
    Postgres(Net, String)          // carried cap + a table-filter policy field

impl Postgres:
    pub fn table(name: String) -> PgPolicy:      // the policy VALUE, type-owned
        PgPolicy(name)

// usage, entirely in the library's own vocabulary:
let readonly = pg.only(Postgres.table("users"))
```

The verb (`pg.only`) and the policy constructor (`Postgres.table`) now both live
with the capability — no shared `confine`, no cross-module reach.

## Non-goals

- **No verb rebrand.** `only` / `deny` stay. `net.default_deny().allow(…)` (a
  deny-by-default allowlist idiom) was considered and **deferred**: it is a separate
  axis from *where policy constructors live*, and doubling the change would widen the
  blast radius across the spec and book for no gain to the concern this RFC settles.
  It can be revisited on its own later.
- **No new enforcement.** Policy semantics, the resolved-IP CIDR matching, the
  deny-carry-forward rule, and the two-tier host/library enforcement of RFC-0011 are
  all unchanged. This is a *surface* change.
- **No `Self` / associated types / static trait dispatch.** v1 is only
  "functions namespaced under a type." Those can be later RFCs if a need appears.

## Migration

Pre-prod, one-cut (break-don't-deprecate). A pure rename, mechanizable by `fmt`:

| before (`std/confine`)   | after                     |
|--------------------------|---------------------------|
| `confine.tcp(h, p)`      | `Net.tcp(h, p)`           |
| `confine.any_port(h)`    | `Net.any_port(h)`         |
| `confine.cidr(b, p)`     | `Net.cidr(b, p)`          |
| `confine.cidr_any(b)`    | `Net.cidr_any(b)`         |
| `confine.union(a, b)`    | `Net.union(a, b)`         |
| `confine.private()`      | `Net.private()`           |
| `confine.ext(s)`         | `Dir.ext(s)`              |
| `confine.files()`        | `Dir.files()`             |
| `confine.dirs()`         | `Dir.dirs()`              |

Then delete `std/confine.witchy` and drop `import confine`. Call sites to update
(2026-07-03): `std/http.witchy`, `examples/redis_capability`, and the prose in
`spec/language.md`, `spec/capabilities.md`, `book/src/capabilities-narrowing.md`,
`book/src/appendix-recipes.md` (~36 references, mostly docs). Parity: a
differential test that a policy built via `Net.tcp(…)` narrows identically on both
backends, plus the existing `dir_only_ext_policy_confines_on_both_backends` /
`net_only` suites re-pointed at the new constructors.

## Security considerations

Policy constructors are pure value builders with an **empty capability footprint** —
importing/using them exposes types, never authority (this is preserved verbatim from
`std/confine`). The associated-function mechanism is namespacing; it grants nothing.
`Net.private()` and the CIDR forms keep matching against the **resolved** IP at
connect time, so the SSRF / DNS-rebinding floor (RFC-0020) is unchanged. Because the
host-side enforcement (`net_only` / `dir_admits`) is not touched, there is no new
trust boundary and no way for the surface change to widen a footprint. The one thing
to get right in review: the `Type.fn` resolution rule (§3) must never let a
*value*-shaped `A.b` silently resolve to a same-named type's associated function —
rule 1 (local value) precedes rule 2, and the case is worth a targeted test.

## Alternatives considered

- **Per-capability std modules** (`net.tcp` as a module function, `dir.ext` in a
  `dir` module). Collides with the instance-method namespace — `net.only` is a verb
  on a `Net` *value*, `net.tcp` would be a function in a `net` *module*; two meanings
  of `net.` at a call site is worse than `confine`. Rejected.
- **Refinement verbs that take the primitive args directly** (`net.only_tcp(h, p)`,
  `net.deny_private()`). A method zoo — one verb per policy shape — which is the exact
  per-operation special-casing the project forbids (CLAUDE.md, "optimizations
  generalize"). Rejected.
- **Keep `std/confine`.** The status quo; the smell this RFC exists to remove.
- **Verb rebrand to `default_deny().allow()`.** Deferred, not rejected — see
  *Non-goals*.

## Rollout

Design-first (this document). No code lands until the `Type.fn` associated-function
feature and the resolution rule (§3) are reviewed — they are a language-surface
change with capability-security weight. Implementation, once approved, is: parser
(inherent `impl Type:` blocks + `Type.fn(…)` call form), typeck/linker (register and
resolve associated functions per §3), the `std/confine` → `impl Net:`/`impl Dir:`
move, and the `fmt`-driven call-site migration — each a separate, tested commit,
parity-checked on both backends.
