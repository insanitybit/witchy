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

Narrowing is a typed, monotone downgrade — you can only *drop* rights:

```
read_only(d: Dir[Read+Write]) -> Dir[Read]      # ok: drops Write
connect_only(n: Net[Connect+Listen, t]) -> Net[Connect, t]
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
- Narrowing builtins `read_only`/`write_only` are monotone attenuations (you may
  only keep a right you hold); they are the identity at runtime since rights are
  type-level only.

Still **future** ticks: `Net[Connect/Listen]` + transport split, and surfacing
the right-set in the `caps`/`caps-diff` footprint.

## Open questions

- Whether to ship `Dialer`/`Listener-grant` aliases for the common `Net` cases
  alongside the parameterized form (deferred with the `Net` split).

## Implementation order (non-breaking, incremental)

1. Typechecker: `Ty::Dir(Rights)` / `Ty::Net(Verbs, Transport)`; bare = full;
   parse `Dir[..]`/`Net[..]`; key the ops; add narrowing builtins.
2. Interpreter: carry rights on the cap values; enforce verb at the op boundary
   (defense in depth) and reject unimplemented transports.
3. `capabilities.rs` footprint: report right-sets; `caps-diff` widen-on-gained-right.
4. Example + tests; tighten std pins.