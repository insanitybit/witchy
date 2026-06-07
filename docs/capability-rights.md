# Rights-parameterized capabilities (design)

Decompose each host capability *by type* into the operations it permits, so the
footprint distinguishes verbs and attenuation is statically checked. This is the
enforcement-backed complement to brands (which only record intent).

## Model

A capability type carries a **right-set**:

- `Dir[r]`, `r ⊆ {Read, Write}` — `ReadDir = Dir[Read]`, `WriteDir = Dir[Write]`.
- `Net[v, t]`, verbs `v ⊆ {Connect, Listen}`, transport `t ⊆ {Tcp, Udp, Uds}`.

Bare `Dir`/`Net` mean the **full** right-set (back-compat: existing programs are
unchanged, they just hold the maximal capability).

## Operations keyed to rights (enforcement)

- `read(d: Dir[r], ..)` requires `Read ∈ r`; `write` requires `Write ∈ r`.
- `connect(n: Net[v,t], ..)` requires `Connect ∈ v`; `listen` requires `Listen ∈ v`.

A `Dir[Read]` passed to `write` is a **compile-time** type error — that's the
enforcement (a read-only handle structurally cannot write).

## Static attenuation

Narrowing is a typed, monotone downgrade — you can only *drop* rights — written
with the `as` ascription:

```
let ro = dir as Dir[Read]          # ok: drops Write
let c  = net as Net[Connect, Tcp]  # ok: drops Listen + Udp/Uds
let x  = (net as Net[Connect]) as Net[Listen]   # error: not a subset
```

You cannot widen (add a right) without a fresh grant — rights are unforgeable
like the capability. So "what can this code do?" is read directly off the types
that flow through it.

## Footprint integration

`caps` reports the exact right-set:

```
fetch  Net[Connect, Tcp]
serve  Net[Listen, Tcp]
load   Dir[Read]   (refined: ConfigDir)     # composes with brands
```

`caps-diff`: a **gained** right (Connect → Connect+Listen, Read → Read+Write) is
a WIDENING that fails the gate; a **dropped** right is a safe narrowing. The
supply-chain signal becomes verb-precise — "this dependency now *listens* / now
*writes* files."

## Representation (chosen: rights-as-set on the cap type)

In the typechecker, `Ty::Dir`/`Ty::Net` gain a right-set (small bitset), instead
of staying atomic. Surface syntax: `Dir[Read]`, `Net[Connect, Tcp]`; bare
`Dir`/`Net` parse to the full set. Optionally expose `ReadDir`/`WriteDir` as
aliases for ergonomics.

(Alternative considered — rights as positional generic markers `Dir(Read)` —
reuses existing machinery but encodes a *set* as arity, which is awkward; the
bitset is cleaner.)

## Scope (this milestone)

- Implement verbs/rights end-to-end: `Dir{Read, Write}`, `Net{Connect, Listen}`,
  transport `{Tcp}`.
- `Udp`/`Uds` are **type-level markers only** — the footprint can express
  `Net[Connect, Udp]`, but the runtime returns "transport not implemented." This
  keeps the *taxonomy/auditing* complete even though the transport isn't.
- `write` is greenfield (no `write` builtin exists yet) — introduce it requiring
  `Write`.

## Migration

- Bare `Dir`/`Net` = full rights → all existing examples/std compile unchanged.
- The std footprint pins tighten: `http`/`server` become `Net[Connect/Listen, Tcp]`.
- Brands layer on top: `type LogDir(Dir[Write])`.

## Status

**`Dir` rights are implemented** (typechecker + interpreter + parser):

- Surface syntax `Dir[Read]` / `Dir[Write]` / `Dir[Read, Write]`; bare `Dir` =
  full set. (Chose brackets over `Dir(Read)` to avoid the constructor/generic
  collision; no nominal `ReadDir`/`WriteDir` aliases — `Dir[Read]` reads fine.)
- Ops are gated by right-membership in `Typeck::check_dir_op`: `read`/`exists`/
  `subdir` need `Read`, `write` needs `Write`. A `Dir[Read]` passed to `write`
  is a compile-time error.
- Narrowing is done with the `as` ascription (see below), not per-right builtins.

**`Net` verbs are implemented** (typechecker + interpreter + parser):

- Surface syntax `Net[Connect]` / `Net[Listen]` / `Net[Connect, Listen]`; bare
  `Net` = full set (back-compat).
- Ops gated by verb-membership in `Typeck::check_net_op`: `connect` needs
  `Connect`, `listen` needs `Listen`. `restrict` is verb-neutral address
  attenuation (preserves the verb-set). A `Net[Connect]` passed to `listen` is a
  compile-time error.
- Narrowing is done with the `as` ascription (see below).
- Unrecognized bracket markers (a future transport like `Tcp`) are parsed but
  ignored, so the syntax is forward-compatible with the transport dimension.

**The footprint is right-precise** (`witchy caps` / `caps-diff`):

- `capabilities::analyze` carries a `CapSet` (capability → union of rights), so
  `caps` shows `fetch  Net[Connect]` / `load  Dir[Read]`; bare = full renders as
  the plain name. Brands still compose (`(refined: ConfigDir)`) and a brand
  audits at the rights of the capability it wraps (`LogDir(Dir[Write])`).
- `caps-diff` is verb-precise: a *gained* right (Connect → Connect+Listen, Read →
  Read+Write) is a WIDENING that fails the gate (exit 2); a *dropped* right is a
  safe narrowing. The supply-chain signal becomes "this dependency now listens /
  now writes files," not just "now uses Net."

**The package-manager gate is rights-aware too** (`pm/footprint.rs`):

- The `coven`/`runes` footprint stores capabilities as strings (`Net[Connect]`,
  bare = full), so manifests/lockfiles are unchanged and a legacy bare `Net`
  still means full. The verb-precision lives in two primitives — `cap_difference`
  (rights-aware widening) and `covers` (rights-aware grant coverage) — used by
  `widening_over`, `check_declared`, and the gate's blocking/contributor logic.
- So a dependency upgrade from `Net[Connect]` to `Net[Connect, Listen]` is a
  blocked widening (`--allow-cap Net` clears it), a tightening to `Net[Connect]`
  is free, and an under-declared `[capabilities]` contract is caught per-verb.

**The `Net` transport axis is implemented** (type level; TCP-only runtime):

- `NetRights` carries verbs *and* transports (`Tcp`/`Udp`/`Uds`). Each axis
  defaults to full independently: `Net[Connect]` keeps all transports, `Net[Tcp]`
  keeps all verbs, `Net[Connect, Tcp]` narrows both.
- Only TCP is implemented, so `connect`/`listen` require `Tcp`; a `Net[…, Udp]`
  passed to `connect` is a compile error ("only implemented over `Tcp`"). `Udp`/
  `Uds` thus remain type-level markers that keep the taxonomy expressible/auditable.

**Narrowing is native, via the `as` ascription** (one construct for every axis):

- `cap as Type` re-types a capability to a *subset* of its rights — `net as
  Net[Connect]`, `dir as Dir[Read]`, `net as Net[Connect, Tcp]`. Checked in
  `Typeck::check_narrow`: the target's rights must be a subset of the source's,
  so `as` can only *drop* rights, never widen or cross capabilities. Identity at
  runtime (rights are type-level). This replaced the seven per-right `_only`
  builtins (`read_only`/`write_only`/`connect_only`/`listen_only`/`tcp_only`/
  `udp_only`/`uds_only`), which didn't scale.
- **Implicit** directional narrowing at call boundaries (`Typeck::coerce_arg`): a
  broader capability satisfies a narrower parameter — a full `Net` flows straight
  into a `Net[Connect]` argument, no `as` needed. The callee stays type-bounded
  to its declared rights, so it cannot re-pass more authority than it admits to
  (re-widening a `Net[Connect]` to a full `Net` parameter is rejected — the type
  ceiling holds). So `as` is now only needed when *naming* a narrowed value.

The **CLI footprint** (`witchy caps`/`caps-diff`) surfaces transport too: a
TCP-pinned client audits as `Net[Connect, Tcp]` (distinct from a transport-
agnostic `Net[Connect]`), and gaining a transport (`Net[Connect, Tcp]` →
`Net[Connect]`, which opens up to `Udp`/`Uds`) is a widening. Both axes default
to full independently, and `show_cap` omits a full axis so the output stays terse.

The **package-manager** footprint (`pm/footprint.rs`) tracks transports too: a
TCP-pinned dependency audits as `Net[Connect, Tcp]`, and an upgrade that opens
the transport axis (gains `Udp`/`Uds`) is a blocked widening. The gate's blocking
is computed as `new − (old ∪ allowed)` directly from the real footprints, never
by re-differencing a rendered delta — otherwise a delta like `Net[Listen]` would
re-parse with its transport axis re-expanded to full and could spuriously block.

Known approximation: the footprint stores a *flat* set of axis markers, not the
verb×transport matrix, so a union of two grants narrowed on *different* axes
(e.g. `Net[Connect, Tcp]` + `Net[Listen, Udp]`) over-approximates to their flat
union. This is harmless under the TCP-only runtime (a non-TCP `connect`/`listen`
can't compile anyway), and tracking the full matrix would be overkill for it.

## Open questions

- Whether to ship `Dialer`/`Listener-grant` aliases for the common `Net` cases
  alongside the parameterized form.

## Implementation order (non-breaking, incremental)

1. Typechecker: `Ty::Dir(Rights)` / `Ty::Net(Verbs, Transport)`; bare = full;
   parse `Dir[..]`/`Net[..]`; key the ops; add narrowing builtins.
2. Interpreter: carry rights on the cap values; enforce verb at the op boundary
   (defense in depth) and reject unimplemented transports.
3. `capabilities.rs` footprint: report right-sets; `caps-diff` widen-on-gained-right.
4. Example + tests; tighten std pins.