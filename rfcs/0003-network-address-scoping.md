---
rfc: 0003
title: Value-level network address scoping (host-enforced)
status: implemented
created: 2026-06-21
superseded-by:
tracking: "shipped 2026-06-21 as the `restrict` op + shared `address_admits`/`resolve_admitted` matcher"
---

# RFC-0003: Value-level network address scoping (host-enforced)

> Provisional syntax. Code blocks here are intentionally **not** tagged `witchy`
> so the doc-examples test does not try to compile them.
>
> **2026-06-21 — implemented.** The operation ships as the existing **`restrict`**
> builtin (not a new `peer`): `restrict(net, addr) -> Net` already existed as
> verb-neutral address attenuation and already carried the allowlist on the `Net`
> *value* (`Value::Net(Vec<String>)` / the WASM `nets` handle table) with
> enforcement on `connect`/`try_connect`/`listen` on both backends. This RFC's new
> work landed: a shared **`address_admits`** pattern matcher (exact / `host:*` /
> IPv4 CIDR) in `src/capabilities.rs`, wired into every op on both backends, and a
> **rebinding-safe `resolve_admitted`** for `connect`/`try_connect` (a CIDR/IP
> allowlist is matched against the *resolved IP*, and the dial is pinned to that
> address). `restrict`-to-exact and the launch-grant ceiling were already in place.

## Summary

Give `Net` the **scope** attenuation axis it is currently missing — the network
analog of `subdir` for `Dir`. A new value-level operation, `peer(net, pattern)`,
returns a `Net` confined to a subset of *addresses*, **enforced by the host** at
connect/listen time. This lets one part of a program hand a different part (a
library, a spawned task) a network capability that can reach *only* specified
peers, with the same hard guarantee that a `Dir[Read]` rooted by `subdir` can read
*only* its subtree. The address bound is a **runtime value, not part of the type**
(consistent with witchy's "kind in the type, specific resource at runtime"),
enforced on both backends, and it only ever *shrinks* the reachable set.

This is the host-enforced complement to the library-enforced bounding that
[`RFC-0002`](./0002-user-definable-capabilities.md) already makes possible, and it
extends the rights model in [`capability-rights.md`](./capability-rights.md) with
the scope axis `Dir` has and `Net` lacks.

## Motivation

### `Net` is missing the axis `Dir` already has

Every host capability has two independent attenuation axes:

| Capability | Verbs (in the **type**, audited) | Scope (a **runtime** value, host-enforced) |
|---|---|---|
| `Dir` | `Dir[Read]` / `Dir[Write]` | `subdir(d, "uploads")` — confined to a subtree |
| `Net` | `Net[Connect, Tcp]` / `Net[Listen]` | **— nothing —** |

`Dir` lets you narrow a handle down to one subtree as an ordinary value
operation; the runtime then refuses any read outside it (`read(sub, "../x")` is
denied even though the type is still `Dir`). `Net` has no equivalent: you can
narrow its *verbs* (`Net[Connect]`) and *transport* (`Net[Tcp]`), but every `Net`
that can connect at all can connect to **any** address the program-wide launch
grant (`--net <host:port>`) allows. There is no way to say "this handle may reach
*only* the Redis server" as a value you pass on.

### The launch grant is too coarse

`witchy sandbox --net 10.0.0.5:6379 --net api.example.com:443 app` allowlists two
peers **for the whole program**. But a program typically wants to give its `redis`
dependency reach to `10.0.0.5:6379` *and nothing else*, and its `api` client reach
to `api.example.com:443` *and nothing else* — so a bug or a compromise in one
cannot phone the other (or exfiltrate to a third host it was never meant to touch).
That is least-authority *within* the program, and the launch grant — a single
global gate — cannot express it. `Dir` already solves the filesystem version of
this with `subdir`; this RFC solves the network version.

### Why host-enforced, not just library-enforced

RFC-0002 already lets a `redis` library bound reachability *by interface*: a sealed
`Redis` capability that only ever dials the server internally and never exposes the
raw `Net`. That bounds the **holder** of `Redis` un-bypassably — but the library
*itself* still holds a full `Net` and is trusted not to misuse it, and the
footprint audits as full `Net[Connect, Tcp]`. For an *untrusted* dependency, you
want the bound enforced by the **host**, so even a malicious library physically
cannot open a socket to a disallowed address. That is what `peer` adds, and the two
compose (see *Composition*).

## Design

### The operation

```
peer(net: Net, pattern: String) -> Net
```

`peer` returns a new `Net` value whose reachable-address set is the current set
**intersected** with `pattern`. It is a pure attenuation, exactly like
`subdir`/`as Dir[Read]`: it can only ever *shrink* the set, never widen it.

```
fn main(console: Console, net: Net):
    // hand each dependency only the peer it needs
    let to_redis = peer(net, "10.0.0.5:6379")
    let to_api   = peer(net, "api.example.com:443")
    redis.use(to_redis)      // can reach ONLY 10.0.0.5:6379
    api.use(to_api)          // can reach ONLY api.example.com:443 — not the DB
```

A connect (or listen) on a `Net` whose scope does not admit the target address is
a **runtime error, loud and identical on both backends** — the same discipline as
reading outside a `Dir` subtree.

### Scope is carried by the value (like a `Dir`'s root)

Today a `Net` capability value is a single global gate; the `--net` allowlist is
launch configuration, not per-value state. This RFC makes a `Net` **value carry its
own reachable-address set**, exactly as a `Dir` value carries its root subtree.
`peer` produces a new value with a narrower set; a connect is checked against *that
value's* set.

- Interpreter: the `Net` value holds the address set; `connect`/`try_connect`/
  `listen` check the target against it before touching a real socket.
- Compiled/WASM: the `Net` is a host-side handle (as `Secret` already is); the
  host's connect path checks the handle's set. Parity: both refuse the same
  addresses with the same error.

### The launch grant remains the hard ceiling

Value-level scopes never *widen*. The effective allowlist for any connect is:

```
launch --net grant   ∩   every peer() applied to this Net value
```

So `peer(net, "evil.example.com:443")` on a program launched without that peer in
`--net` still cannot reach it — the launch grant is the outer ceiling and `peer`
only carves *within* it. Running unsandboxed (`witchy app.witchy --net …`) uses the
same intersection; with no `--net` at all the top-level set is empty (no network)
unless the host grants one, so `peer` of an empty set stays empty.

### The pattern language

A `pattern` is `host:port`, where each side may be exact or a wildcard, plus CIDR
for hosts. v1 (minimal, extensible later):

| Pattern | Matches |
|---|---|
| `10.0.0.5:6379` | exactly that IP and port |
| `10.0.0.5:*` | that IP, any port |
| `10.0.0.0/24:6379` | any IP in the CIDR block, that port |
| `10.0.0.0/24:*` | any IP in the block, any port |

`*:*` is the unrestricted top and is only meaningful as the launch-grant default;
`peer` arguments must name at least a host (no widening to `*`). Narrowing composes
by intersection — `peer(peer(net, "10.0.0.0/24:*"), "10.0.0.5:6379")` yields
`10.0.0.5:6379`. (A future revision may accept a *list* of patterns for a handle
that legitimately needs a small fixed set; v1 keeps it one pattern per `peer`,
chained for unions only when each is a subset — i.e. no widening.)

### Hostnames and DNS rebinding (the security subtlety)

If a pattern names a **DNS host** (`api.example.com:443`) rather than an IP, a
hostile or compromised resolver could later resolve that name to an arbitrary
address — the classic DNS-rebinding escape. The enforcement therefore checks the
**post-resolution IP actually being connected to** against the scope, not the name
the program typed:

- A pattern given as an **IP/CIDR** is matched against the connect target's IP
  directly — the strong, rebinding-proof form.
- A pattern given as a **hostname** is resolved by the **host** at connect time,
  and the resulting IP must itself fall within the scope's IP set; the name is a
  convenience, the IP is the gate. (Equivalently: a hostname pattern pins to the
  set of IPs it resolves to *at grant time*, and a connect must land in that set.)

The RFC's recommendation for untrusted dependencies is **IP/CIDR patterns**, which
carry no resolver trust. Hostname patterns are allowed for ergonomics with the
resolve-and-recheck rule above; the spec must state which semantics ship.

### Connect-side vs listen-side

The scope is a set of address patterns; *which* address it constrains depends on
the verb:

- For `connect` / `try_connect` (a `Net[Connect]`), the **remote target** must
  match — "you may dial only these peers." This is the common case.
- For `listen` (a `Net[Listen]`), the **local bind address** must match — "you may
  bind only here." Useful to pin a server to one interface/port.

Most uses scope a connect-only handle. The two are not conflated: a `peer` on a
`Net[Connect, Listen]` constrains both the dial target and the bind address to the
same set, which is the conservative reading; a later revision may split them if a
real need appears.

### Unix domain sockets unify with `Dir`

For `Net[…, Uds]`, the "address" is a **socket path**, so scoping a UDS `Net` is a
*path* allowlist — structurally the same operation as `subdir`. v1 may scope UDS by
exact path; this is a natural place for the `Dir` and `Net` scope axes to share
machinery, and the RFC notes it without requiring it in the first cut.

### Composition with user capabilities (RFC-0002)

`peer` and RFC-0002 stack into defense in depth: a `Redis` capability mints **from
a host-scoped `Net`**, so it is bounded by the host (cannot reach other IPs even
inside the library) *and* by interface (the holder gets only redis ops, no raw
`Net`).

```
fn main(console: Console, net: Net):
    let bounded = peer(net, "10.0.0.5:6379")      // host-enforced
    match redis.connect(bounded, "10.0.0.5", 6379):   // library-enforced interface
        Ok(db) -> ...                             // db: Redis, doubly confined
```

### Footprint and `caps` — the honest limitation

The address bound is a **runtime value**, so — exactly like a `Dir`'s subtree — it
is **not** in the type and **not** in the footprint. `witchy caps` reports
`Net[Connect, Tcp]` whether or not the handle has been `peer`-narrowed; `caps-diff`
does not see an address change. The statically-audited ceiling stays the launch
`--net` grant (the program as a whole cannot exceed it); `peer` is a *runtime*
confinement *within* that ceiling.

This is the consistent witchy tradeoff (kinds are static and audited; specific
resources are runtime and enforced), and it is the right one — addresses are often
dynamic (per-tenant, per-config, computed). If a program wants a *compile-time*
distinct handle, it wraps the scoped `Net` in a sealed facet (RFC-0002), e.g.
`capability RedisNet from Net[Connect, Tcp]` minted from `peer(net, …)`; the facet
documents intent in the type, the IP remains a runtime confinement. The RFC does
**not** attempt to put addresses in types.

### Firewall, errors, and ops affected

- A scoped `Net` is still a `Net`; `retain net` / `without net` treat it uniformly.
- Connect/listen outside the scope raises a loud, parity-identical runtime error
  naming the denied address and the handle's scope (mirrors a `Dir` out-of-subtree
  read). `try_connect` returns the corresponding `Err` rather than aborting.
- Ops affected: every op that takes an address — `connect`, `try_connect`,
  `listen` (and, for the stdlib `http`/`server` clients, the `host`/`addr`
  arguments they forward). `http.get(net, host, port, path)` over a scoped `net`
  succeeds only if `host:port` is in scope.

### Type-system / runtime changes

- A `Net` value gains an **address-scope** field — a set of patterns; default
  "unrestricted" (bounded by the launch grant). This mirrors `Dir`'s carried root.
  Net's *type* is unchanged (`Ty::Net(NetRights)`); scope lives in the value, not
  the type.
- A new builtin `peer : (Net, String) -> Net`, type-checked alongside
  `connect`/`restrict`/`connect_only` in `check_net_op`, returning a `Net` with the
  same rights and an intersected scope.
- The interpreter `Net` value and the WASM host-side `Net` record both carry the
  scope; the connect/listen paths consult it (plus the launch ceiling).
- A small address-matcher (exact / wildcard-port / CIDR; hostname-resolve hook)
  shared by both backends so matching is identical.

## Alternatives

- **Do nothing — rely on launch `--net` + RFC-0002.** You keep a coarse global
  ceiling and library-*interface* bounding, but never host-enforced *per-handle*
  address confinement for untrusted code. Rejected: leaves the POLA-within-a-program
  case (give each dependency only its peer) unsolvable with a hard guarantee.
- **Put the address in the type (`Net[to=10.0.0.5]`).** Rejected: addresses are
  runtime, often dynamic; encoding them in types causes type explosion and a
  static/runtime mismatch witchy deliberately avoids everywhere else (`Dir` doesn't
  put paths in types either).
- **Library/user-capability bounding only (RFC-0002).** That is the soft,
  trust-the-library form. This RFC is the hard, host-enforced complement; they
  compose rather than compete.
- **Hostname allowlists as the primary form.** Rejected as the *default* for
  untrusted code (DNS rebinding); IP/CIDR is the rebinding-proof primary, hostnames
  are an ergonomic option with resolve-and-recheck semantics.
- **A full egress-policy DSL** (protocols, rate limits, time windows). Out of scope
  — those are library/runtime policy (the caretaker pattern in `secrets-design.md`),
  not a host capability primitive. Keep `peer` to address-set intersection.

## Drawbacks

- **`Net` values gain carried state.** A `Net` is no longer a bare gate; it holds a
  scope on both backends. Small, but it is representational surface that must stay
  parity-identical.
- **The bound is not statically auditable.** `witchy caps` cannot show "reaches only
  10.0.0.5" — it is a runtime confinement, consistent with `Dir` subtrees but a real
  limitation if you wanted the bound in the footprint. (RFC-0002 facets recover a
  *type-level* marker, not the address.)
- **DNS-rebinding correctness is load-bearing.** The resolve-and-recheck path must
  be right, or hostname patterns become an escape. The conservative default
  (IP/CIDR) mitigates, but the hostname path needs care and tests.
- **Pattern-language scope creep.** CIDR + wildcards + (later) lists invite
  feature growth; v1 is deliberately minimal and must resist becoming a firewall
  config language.
- **Connect/listen asymmetry.** Using one scope for both verbs is a simplification
  that a split (dial-set vs bind-set) may later need to undo.

## Prior art

- `subdir` and `Dir` subtree confinement (`src/typeck.rs` `check_dir_op`) — the
  direct structural analog this RFC mirrors for `Net`.
- [`capability-rights.md`](./capability-rights.md) — the `Net[v, t]` verb/transport
  rights this scope axis sits beside.
- [`RFC-0002`](./0002-user-definable-capabilities.md) — the library-enforced
  bounding this host-enforces and composes with.
- Object-capability attenuation (POLA); OS egress allowlisting / iptables CIDR
  rules; Deno's launch-level `--allow-net=host:port` (this RFC's value-level,
  host-enforced generalization of that idea).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
