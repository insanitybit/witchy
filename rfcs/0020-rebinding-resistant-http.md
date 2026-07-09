---
rfc: 0020
title: DNS-rebinding-resistant HTTP — resolve / pin / connect + private-range confine
status: implemented
created: 2026-06-27
implemented: 2026-07-02 (all layers: 0–1 the resolve-once invariant + confine.private();
  2 the resolve/connect_pinned primitives; 3 the sealed PinnedUrl fetch surface)
superseded-by:
tracking:
---

# RFC-0020: DNS-rebinding-resistant HTTP — resolve / pin / connect

> Code blocks here are intentionally **not** tagged `witchy` (per RFC-0002's
> convention): they are illustrative sketches, not complete programs, and must
> not be executed by the doc-test harness.

> **Status: implemented** (2026-07-02). All layers shipped; behavior lives in
> `spec/capabilities.md`, `std/http`/`std/confine`, and the code. This RFC is frozen.
> - **Layer 0** — the resolve-once-and-pin invariant is documented in
>   `spec/capabilities.md` (connect resolves once; the checked IP set is the
>   dialed set; CIDR/IP entries match the resolved IP, bare hostnames don't).
> - **Layer 1** — `confine.private()` in `std/confine`. `net.deny(confine.private())`
>   refuses an internal address at connect time, enforced on the resolved IP. A silent
>   IPv6 gap in the matcher (its `::1/128`/`fe80::/10`/`fc00::/7` ranges only ever
>   exact-matched) was closed — `address_admits` now CIDR-matches IPv6.
> - **Layer 2** — `net.resolve(host) -> List(String)` and
>   `connect_pinned(net, ip, host, port, secure)` / `try_connect_pinned` on both
>   backends (new host ops, `IMPORT_COUNT` bumped). `resolve` performs no allowlist
>   filtering; `connect_pinned` dials the literal IP and re-checks it against the Net
>   allowlist (the hard floor), presenting the hostname for SNI/`Host`.
> - **Layer 3** — a sealed `PinnedUrl` and `pin`/`unpinned`/`get_pinned`/`send_pinned`
>   in `std/http`. **Realization note:** witchy has no sealed SUM type, so `PinnedUrl`
>   is an EAGER sealed RECORD (`capability PinnedUrl`, invariant: always a vetted pin)
>   rather than the `Unresolved | Resolved` enum sketched in the Design below. This is
>   strictly safer — no unvetted state, no closure stored in a sealed value — and keeps
>   the security authority intact: resolve-once, an unforgeable proof-carrying value,
>   and the allowlist as the hard floor. The chooser is a plain `fn(String) -> Bool`
>   predicate argument to `pin` (not a stored `fn`-field), so the "sealed enum carrying
>   a closure" round-trip concern the Drawbacks flag does not arise. Proven both
>   backends: `net_resolve_and_connect_pinned_backends_agree`,
>   `connect_pinned_rechecks_the_allowlist_backends_agree`,
>   `net_deny_private_blocks_internal_ipv6_on_both_backends`,
>   `http_pin_and_get_pinned_backends_agree`.
>
> The `type PinnedUrl:` sum, `net.connect_pinned(ip, host, port, secure)` free-function
> shape, and `pin_with`/`public_only` in the Design section below are the ORIGINAL
> sketch; the shipped surface is as summarized above (see `spec/stdlib.md`).

## Summary

Make it *easy* to write an HTTP client that is immune to DNS-rebinding / SSRF
"check-then-reconnect" attacks. We already resolve-once-and-pin in the `Net`
capability layer; this RFC exposes that property to witchy programs in two ways:
a one-line **capability** defense (`net.deny(confine.private())`, enforced on the
resolved IP), and a **dynamic** primitive (`net.resolve` + `net.connect_pinned`)
surfaced through a sealed, proof-carrying **`PinnedUrl`** type. `PinnedUrl` is an
enum of `Unresolved` (a URL plus a policy that *will* run) and `Resolved` (a host
name plus the single IP that *was* checked); the HTTP client connects to *that
exact IP* — keeping the original hostname for TLS SNI and the `Host` header — with
no second resolution able to slip a different address underneath it. A program opts
into the behavior by handing the client a `PinnedUrl` instead of a bare URL, and a
`Resolved` value cannot be forged, so its *type* is the proof that policy ran.

## Motivation

The classic SSRF foot-gun is a time-of-check/time-of-use gap:

```
ip = resolve(url.host)         # 1. resolve
if is_external(ip): ...        # 2. check — looks safe
fetch(url)                     # 3. use — re-resolves; attacker rebound the name to 169.254.169.254
```

Between step 2 and step 3 the name is re-resolved, and an attacker who controls
the authoritative DNS for the host returns an internal address the check never
saw. The request hits the cloud metadata endpoint, an internal admin panel, etc.

witchy is unusually well-placed to make this hard, because the network entry point
is already a capability, and the capability layer **already** resolves a name once
and dials the exact resolved addresses:

- `resolve_admitted(allow, host)` (`crates/witchy-caps/src/capabilities.rs`)
  resolves the name, filters the resolved IPs against the allowlist, and returns
  concrete `SocketAddr`s.
- `dial(targets, tls, host_port)` (`crates/witchy-runtime/src/net.rs`) connects to
  exactly those addresses — it never re-resolves — and validates the TLS
  certificate against the original host name.

So `net.connect("host:port")` is *already* rebinding-safe for any policy that the
**capability allowlist** expresses. What's missing is the ability for a *program*
to (a) see the resolved IP and apply its own policy, and (b) pin a checked IP
through a real HTTP request without breaking TLS. Today there is no `resolve`
primitive at all, and no way to say "dial `1.2.3.4` but present `example.com` as
SNI and `Host`." A program that wants the pattern above literally cannot write it,
so people will reach for the broken `check-then-fetch` shape — or, worse, do
nothing.

We want the *safe* version to be the *easy* version.

## Design

Four layers, smallest blast-radius first. Layers 0–1 cover the common case with no
new primitives a program has to reason about; layers 2–3 cover dynamic policy.

### Layer 0 — state the guarantee we already have

The connect path resolves once and dials the exact resolved IPs. We will document
this as an explicit, tested invariant in `spec/capabilities.md`:

> **Resolve-once-and-pin.** `connect`/`try_connect` resolve a hostname a single
> time; the IP set checked against the `Net` allowlist is the *same* set dialed.
> No code path re-resolves between the check and the connection.

One sharp edge to document alongside it: a **hostname** allowlist entry
(`example.com:443`) is matched by *name* and falls back to dialing all of the
name's resolved addresses (`resolve_admitted` `name_ok` branch). That form is an
ergonomic convenience and is **not** rebinding-proof. The rebinding-proof forms are
IP and CIDR entries, which are matched against the resolved address.

### Layer 1 — `confine.private()`: the one-line capability defense

Most SSRF guards want "deny the internal ranges." That is a *static* policy, so it
belongs in the capability, where `resolve_admitted` already enforces it on the
resolved IP. Add to `std/confine`:

```
// A NetPolicy matching the non-public IP ranges (loopback, RFC-1918, link-local,
// unique-local, CGNAT, metadata). `net.deny(confine.private())` confines a Net so
// it can never dial an internal address — enforced on the RESOLVED IP, so a name
// that rebinds to an internal address is refused at connect time.
pub fn private() -> NetPolicy
```

`private()` is a `confine.union` of (at least):

- `127.0.0.0/8`, `::1` — loopback
- `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` — RFC-1918
- `169.254.0.0/16`, `fe80::/10` — link-local (incl. the `169.254.169.254` metadata IP)
- `fc00::/7` — IPv6 unique-local
- `100.64.0.0/10` — CGNAT
- `0.0.0.0/8` — "this host"

Then the whole defense is:

```
let safe = net.deny(confine.private())     // hand `safe` to untrusted-URL code
```

**Two prerequisites this exposes** (both are real gaps today, fixed as part of
this work):

1. **`net.only` must preserve `deny` entries under further narrowing.** The
   refinement algebra currently rebuilds the allowlist from the new *positive*
   patterns only (`net_narrow_to` / `host_net_restrict`), silently dropping any
   `!`-deny entries — so a later `net.only(superset)` re-opens a denied range.
   `net.deny(confine.private())` is only sound once `only` carries denies forward
   (intersection that keeps the exclusion set). This is a prerequisite, not an
   optional extra.
2. **CIDR matching must cover IPv6.** `address_admits` / `parse_ipv4_cidr`
   (`crates/witchy-caps/src/capabilities.rs`) only understand IPv4 CIDR today, so
   the IPv6 ranges in `private()` would not match. Extend the matcher to IPv6
   prefixes (parse `::1`, `fe80::/10`, `fc00::/7`; compare on the 128-bit value).

### Layer 2 — `net.resolve` + `net.connect_pinned`: the dynamic primitive

When the policy is dynamic (a per-tenant allowlist, a logged decision, an
allowlist from a database) the program must see the IP. Expose the two halves of
what the host already does:

```
// Resolve a hostname to its current IP literals. Gated on holding Net[Connect];
// performs no allowlist filtering (the program decides). Resolution adds no
// authority beyond `connect`, because `connect_pinned` below still enforces the
// Net allowlist on the chosen IP.
net.resolve(host: String) -> Result(List(String), String)        // ["93.184.216.34", "2606:2800:220:1:..."]

// Connect to an EXACT ip:port — no DNS lookup — while presenting `host` as the
// TLS SNI / certificate name and the HTTP `Host`. `secure` selects TLS. The Net
// allowlist is still enforced against `ip`, so this can never exceed the
// capability's confinement. A `try_` variant returns Option for the fallible path.
net.connect_pinned(ip: String, host: String, port: Int, secure: Bool) -> Socket
net.try_connect_pinned(ip: String, host: String, port: Int, secure: Bool) -> Option(Socket)
```

The TOCTOU is closed *by construction*: the program passes the **literal IP it
inspected**, and `connect_pinned` never re-resolves. The hostname travels
separately, only to fill SNI and `Host` — so TLS validation and name-based vhosts
still work while the socket goes to the checked address.

```
match net.resolve(host):
    Err(e) -> Err("resolve failed: ${e}")
    Ok(ips) ->
        let ip = pick_checked(ips)          # your policy decides; you keep the literal
        let sock = net.connect_pinned(ip, host, 443, true)   # dials `ip`, SNI = host
        ...
```

`connect_pinned` reuses the existing dialer: it builds the single `SocketAddr` from
`ip:port`, runs the same allowlist check as `connect` (so capability confinement is
unchanged), and calls `dial([ip:port], secure, "${host}:${port}")` — i.e. the SNI
is derived from `host`, not from `ip` (`server_name` in `net.rs`).

### Layer 3 — `PinnedUrl`: policy as a proof-carrying type

The ergonomic surface is a type whose *shape* records whether a policy has run, so
the check can't be silently forgotten. `PinnedUrl` is a **sealed** type (a
`capability`-style record: private constructors, fields reached by `match`, never
`.field`) with two states:

```
type PinnedUrl:
    Unresolved(String, fn(Net[Connect], List(String)) -> Result(String, String))   # url, chooser(net, candidates) -> ip
    Resolved(String, Int, Bool, String)                                            # host, port, secure, pinned_ip
```

- `Resolved` is a concrete, already-checked target: the host name (kept for TLS SNI
  and the `Host` header) plus the single pinned IP. The client dials *exactly* that
  IP via `connect_pinned` and never re-resolves.
- `Unresolved` means "policy will run at send time": a URL plus a chooser. The client
  performs the single `net.resolve`, hands the resolved candidates to the chooser,
  and pins whatever it returns.

Construction — a sugar form for the common pure predicate, a general form for
networked policy, and an explicit no-policy escape hatch:

```
// Common case: keep only IPs the predicate approves, pin the first.
pub fn pin(url: String, allow_ip: fn(String) -> Bool) -> PinnedUrl

// Full control: the chooser SELECTS/validates among the already-resolved candidates
// and may use the Net for auxiliary policy I/O (reverse-DNS, an external allowlist).
// It does NOT resolve — that stays the client's single, pinned step.
pub fn pin_with(url: String, chooser: fn(Net[Connect], List(String)) -> Result(String, String)) -> PinnedUrl

// Explicit "I have no policy here" — names (and makes greppable) the unsafe path.
pub fn unpinned(url: String) -> PinnedUrl

// A ready-made predicate mirroring confine.private(): reject internal ranges.
pub fn public_only(ip: String) -> Bool
```

The client accepts a `PinnedUrl` and honors the pin, supporting both lazy and eager
use:

```
// force the policy, yielding a Resolved (idempotent on an already-Resolved value)
pub fn resolve(net: Net[Connect, Tcp], p: PinnedUrl) -> Result(PinnedUrl, String)

// send, honoring the pin; runs the policy first if still Unresolved
pub fn get_pinned(net: Net[Connect, Tcp], p: PinnedUrl) -> Result(Response, String)
pub fn send_pinned(net: Net[Connect, Tcp], p: PinnedUrl, method: String, headers: List((String, String)), body: String) -> Result(Response, String)
```

```
http.get_pinned(net, http.pin(user_url, http.public_only))    # lazy: resolve+check+pin+connect, one call
let p = http.net.resolve(http.pin(user_url, public_only))?    # eager: now Resolved(host, port, secure, ip)
... inspect / log p ... ; http.get_pinned(net, p)              # connects to the IP already vetted
```

`net.resolve(p)` is where the single resolution happens:

```
match p:
    Resolved(_, _, _, _) -> Ok(p)                              # idempotent; already pinned
    Unresolved(url, chooser) ->
        parse url -> (host, port, secure)
        ips = net.resolve(host)?                               # the ONE resolution
        match chooser(net, ips):                               # select/validate among candidates
            Ok(ip)   -> Ok(Resolved(host, port, secure, ip))
            Err(why) -> Err("blocked by policy: ${why}")
```

and `get_pinned` connects to the pinned target, never re-resolving the host:

```
match net.resolve(p)?:
    Resolved(host, port, secure, ip) ->
        sock = net.connect_pinned(ip, host, port, secure)      # dial `ip`, SNI/Host = host
        send request with `Host: host`; parse response
```

**Why a type, not just a predicate argument.** Because `PinnedUrl` is sealed, the
only way to obtain a `Resolved` is through `resolve`/`pin*` — a caller cannot
hand-forge `Resolved(host, attacker_ip, ...)`. The value itself is the proof that
policy ran: a function that wants a vetted target asks for a `PinnedUrl` and the type
system enforces the rest. The `Net` allowlist still sits underneath as the hard floor
(`connect_pinned` re-checks the IP), so even a buggy or hostile chooser cannot exceed
the capability — the same two-tier "hard host capability + soft sealed policy"
composition as `capability Postgres` in `spec/capabilities.md`.

**Resolution is the client's single step; the chooser only selects.** The chooser
receives the *already-resolved* candidate list and returns one of them (or an `Err`);
it must not resolve again. This keeps the resolve-once invariant in one place and
prevents a chooser from re-resolving at send time and reintroducing the TOCTOU. The
chooser still takes the `Net` so a policy that needs its own network lookups can make
them — but the address dialed is always one the client resolved and the chooser
returned.

**Multi-record policy: select-one.** A benign round-robin host resolves to several
addresses; the chooser returns one (the `pin` sugar filters to the predicate-passing
addresses and returns the first). We deliberately do not "require all pass" — that
would break legitimate multi-A hosts where one address is momentarily unroutable —
and the client never dials an address the chooser didn't return.

### Capability + parity

- `net.resolve` and `net.connect_pinned` are linked **only** under a `Net[Connect]`
  grant, exactly like `net_connect` (`crates/witchy-runtime/src/runtime.rs`
  linker), so they appear in a compiled module's import list and footprint. A
  module with no `Net` cannot resolve or pin-connect.
- Both backends implement both ops (interpreter arms + WASM host imports
  `net_resolve` / `net_connect_pinned`), bumping `IMPORT_COUNT`
  (`crates/witchy-wir/src/wir_prelude.rs`). A differential test in
  `example_tests.rs` exercises resolve → pin → connect against a loopback listener
  to keep the two backends identical.

### Resolved design questions

- **Who resolves, and when?** The client, exactly once, inside `net.resolve(p)`.
  The chooser only *selects/validates* among the candidates the client resolved — it
  must not resolve again — so the resolve-once-and-pin invariant lives in one place.
- **Why does the chooser take `Net` if it doesn't resolve?** For *auxiliary* policy
  I/O (reverse-DNS, querying an allowlist service). It adds no authority: the dialed
  address is always one the client resolved, and `connect_pinned` re-checks it
  against the allowlist.
- **Is `resolve` per-host capability-gated?** No — only on holding `Net[Connect]`.
  The point is to inspect *before* a decision; gating per-host would defeat the
  dynamic case, and it grants nothing new because `connect_pinned` re-checks the
  chosen IP.
- **Opt-in or mandatory?** Opt-in. A bare-URL `get_url(net, String)` stays for
  trusted/constant URLs; untrusted input goes through `pin`/`get_pinned`. The unsafe
  path is *named* (`unpinned`) so it is greppable in review rather than silent.
- **What about redirects?** The client does not auto-follow `Location` today, so a
  pinned fetch is single-hop and safe. If redirect-following is ever added, each hop
  must be re-pinned (resolve + choose + pin per hop); this RFC makes that the
  required shape rather than an afterthought.

## Alternatives

- **Do nothing; rely on capability confinement only.** Layer 0/1 already make the
  *static* case safe, and that is the idiomatic witchy answer. But it gives a
  program no IP visibility and no dynamic policy, and it leaves the hostname-
  allowlist foot-gun unaddressed. Rejected as insufficient for untrusted-URL fetch.
- **Resolve and rewrite the URL to the IP, keep a `Host` header.** This is exactly
  `connect_pinned` *minus* the SNI fix — it breaks TLS (cert won't match the IP).
  Adding the name back for SNI collapses it into this proposal.
- **Host-side global flag (`--deny-private`).** Coarse, invisible in the footprint,
  not composable, and unable to express per-call/dynamic policy. Confinement via a
  typed `NetPolicy` is the witchy-shaped version.
- **A caching/pinning resolver type.** More machinery and state than the problem
  needs; the literal-IP pin already gives a single, inspectable, re-use-without-
  re-resolution handle.
- **A bare guarded function (`get_url_guarded(net, url, allow_ip)`).** The earlier
  draft of this RFC. It works, but the policy is a *parameter* a caller can forget,
  and nothing in the value records that a check happened. `PinnedUrl` keeps that call
  as sugar (`pin` + `get_pinned`) while making "this target was vetted" a sealed,
  unforgeable value — preferred for the same reason capabilities are values, not
  flags.

## Drawbacks

- **New host imports on both backends** — a parity surface to keep in lockstep and
  an `IMPORT_COUNT` bump. Mechanical, but real.
- **IPv6 CIDR support** must be added to `address_admits` for `confine.private()` to
  be complete; until then the helper would silently cover only IPv4 (an honest gap
  we must close, not paper over).
- **`resolve` exposes raw DNS answers** to the program — a minor information surface
  (and a DNS-traffic side channel), bounded by `Net[Connect]`, which already lets
  the holder make DNS queries via `connect`.
- **Sealed enum carrying a closure.** `Unresolved`'s chooser is a `fn` field in a
  sealed variant; sealing + a closure-valued variant must round-trip identically on
  both backends — an implementation check, not a blocker.
- **Opt-in, not mandatory.** `unpinned`/`get_url` still exist, so a misuse mode
  remains: a caller can write their own `check-then-connect(host)` with the
  *un-pinned* `connect`, or reach for `unpinned`. We mitigate by making `PinnedUrl`
  the documented default, `public_only` a one-liner, and the unsafe path *named* and
  greppable — but we cannot delete the lower-level `connect`.
- Does **not** address application-layer SSRF beyond address pinning (e.g. a server
  that itself proxies). Out of scope; the capability still bounds reachable hosts.

## Prior art

- **DNS rebinding / SSRF pinning.** The standard mitigation (OWASP SSRF prevention
  cheat sheet) is "resolve once, validate the address, connect to that address" —
  precisely Layers 2–3.
- **Go `net.Dialer.Control`.** Go's idiomatic SSRF guard hooks `Control(network,
  address, conn)`, invoked with the *resolved* address immediately before connect,
  to reject internal IPs without a re-resolution window. `connect_pinned` + the
  `PinnedUrl` chooser are the witchy analog, with the check lifted into ordinary
  code and the vetted result carried as a sealed value.
- **cap-std / Capsicum.** Capability-oriented networking where authority is a value,
  not ambient — the model `Net` already follows; this RFC keeps the new ops inside
  that model (linked only under the grant, re-checked against the allowlist).
- Builds directly on **RFC-0003** (network address scoping), **RFC-0009** (host-side
  TLS termination — the SNI/cert-name handling `connect_pinned` reuses), and
  **RFC-0011** (capability refinement — the `only`/`deny` algebra `confine.private`
  relies on).

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
