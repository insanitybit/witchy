# capability_rights

Rights-parameterized capabilities, where the footprint distinguishes the *verbs*
a capability permits, not just its kind: a `Dir[Read]` provably cannot write, a
`Net[Connect, Tcp]` is a TCP client that cannot listen, and a `Net[Listen]` is a
server that cannot dial out. Narrowing is native — implicit at a call boundary
(more authority stands in for less) or explicit with `as` — and you can only ever
drop rights, never widen.

**Shows:** the `Dir` and `Net` capabilities, rights/transport markers, implicit
and explicit (`as`) capability narrowing, and `pub` functions.

## Run

```sh
witchy run                                                          # from this directory
witchy examples/capability_rights/src/capability_rights.witchy      # or by file, from the repo root
witchy caps examples/capability_rights/src/capability_rights.witchy   # per-function footprints
```
