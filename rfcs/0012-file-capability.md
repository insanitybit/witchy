---
rfc: 0012
title: File as a first-class capability (Exec fold rejected — Exec stays separate)
status: implemented
created: 2026-06-23
implemented: 2026-06-25 (File[Read|Write] + navigation + direct main grants)
tracking:
---

# RFC-0012: File as a first-class capability

> **Status: implemented** (2026-06-25), with one part **rejected**. Shipped on BOTH
> backends: `File`/`File[Read]`/`File[Write]` as a host capability (right-typed like
> `Dir`, footprint-visible, `as` facets + implicit narrowing); `f.read() -> String` /
> `f.write(data)` leaf ops; `Dir` navigation `dir.read_file(rel) -> File[Read]` /
> `dir.write_file(rel) -> File[Write]`, with the same `..`/absolute/symlink confinement
> as `read`; AND **`main` receiving a `File` directly** — `--file <path>` grants fill
> `main`'s `File` parameters positionally (the i-th `File` param ← the i-th `--file`),
> read/write being the param's compile-time right, so `main(config: File[Read])`
> audits as exactly `Console, File[Read]` with no `Dir`.
>
> **REJECTED: folding `Exec` into `File[Exec]`** (the "Folding Exec…" section below).
> `Exec` stays its own distinct capability. Rationale (2026-06-25): exec is the
> single most severe authority — a spawned process runs with full OS authority
> *outside* the sandbox — and bundling it into `File` as a rights bit makes it easy
> to grant exec accidentally while reaching for "just a file". Keeping `Exec`
> conspicuous forces it to be granted deliberately and audited on its own line. So
> `File` is **read/write only**; the existing `exec.run(e: Exec, dir: Dir[Read], …)`
> (binary named through a `Dir[Read]`, RFC-0004) is unchanged.
>
> Code blocks here are intentionally **not** tagged `witchy` so the doc-examples
> test does not try to compile partial snippets.
>
> Note on names: the navigation ops shipped as **`dir.read_file`/`dir.write_file`**
> (the name states the conferred right); the design below uses the older
> `dir.open`/`dir.create`/`dir.join` sketch. `dir.join` (Dir→Dir) was not built —
> `dir.subtree` (RFC-0011) is the Dir→Dir navigation.

## Summary

Add **`File`** as a host capability with `Read`/`Write`/`Exec` rights — the *leaf* of
the same resource/authority hierarchy as `Dir` (authority to one file vs. one subtree).
`main` can receive `File` capabilities directly (`main(config: File[Read], log:
File[Write], …)`), and a `Dir` produces them by safe navigation (`dir.read_file("x.txt") ->
File[Read]`, `dir.join("sub") -> Dir`, both rejecting `../`/absolute escape). The
ambient **`Exec` capability is folded into `File[Exec]`** — the authority to run a
*named* binary rather than "run anything" — with one prominent, honest caveat: exec
confinement ends at the process boundary. Under RFC-0011's two tiers, `File` is a
host-primitive (host-enforced, visible in `witchy caps`).

## Motivation

- **Single-file needs over-grant today.** The filesystem menu is `Dir` + relative-path
  operations; a program that must read one config file has to be handed a whole `Dir`,
  which is authority over an entire subtree. `File[Read]` expresses "this one file,
  read-only" — the least-authority leaf.
- **`Exec` is ambient.** "Run any program" is the single broadest authority witchy can
  hand out, and it audits indistinguishably regardless of *what* it runs. Naming the
  binary (`File[Exec]`) is more precise and auditable, and RFC-0004 already leans this
  way (the `witchy` CLI drives `witchyc` through a constrained, branded Exec).
- **It completes the hierarchy.** `Dir` is a subtree; `File` is a leaf; both are
  authority-bearing handles under the same model (RFC-0011). The gap is conspicuous.

## Design

### `File[Read | Write | Exec]`

`File` mirrors `Dir`'s right-typing. A `File` is authority to *one* file; it carries no
path-scope to refine further (it is already a leaf), so its only refinement axis is its
**rights** — a facet narrows them:

```
read(f: File[Read]) -> Result(String, String)
write(f: File[Write], data: String) -> Result(Nil, String)

let ro = f as File[Read]      # facet: drop Write — ordinary rights attenuation (RFC-0002)
```

### `main` accepts files (and mixes them with dirs)

Multiple, mixed capability parameters already work — coven-web's `main` takes
`Console + Net + Dir + Secret + Clock` today. This adds `File` to the set the host can
grant:

```
fn main(config: File[Read], log: File[Write], data: Dir[Write], net: Net[Connect, Tcp]):
    …
```

The launch grant names each (`--file-read config.toml`, `--file-write run.log`, …), or a
grant document enumerates them ([RFC-0013](./0013-capability-grant-documents.md)). The host opens each file and hands `main` the
handle; the program never names a path the host did not grant.

### Navigation: `Dir` produces `File` (attenuation-by-naming)

Opening a child *names* a sub-resource and yields a narrower capability — the
"navigation" flavor of RFC-0011's refinement (vs. the policy flavor `dir.only(…)`):

```
dir.join(rel)  -> Result(Dir, String)            # a child subtree (safe: no ../, no absolute)
dir.read_file(rel)  -> Result(File[Read], String)     # a file handle; rights ≤ the Dir's rights
dir.write_file(rel) -> Result(File[Write], String)   # create-and-open within the subtree
```

`join`/`open` are the ergonomic, common-case surface (your "`Dir` has a `.join`");
`dir.only(kind(File), ext(".txt"))` (RFC-0011) is the policy surface. Both yield `≤`
capabilities; they compose (`dir.join("uploads").only(kind(File))`).

### Folding `Exec` into `File[Exec]` — REJECTED

> **This section is rejected** (see the status note). `Exec` stays its own
> capability; `File` is read/write only. The design below is kept for the record.


`File[Exec]` is the authority to spawn *that binary*. The ambient `Exec` capability is
removed; `spawn` takes a `File[Exec]` plus the other capabilities the child needs
(cwd as a `Dir`, environment, stdio), each explicit:

```
spawn(bin: File[Exec], args: List(String), cwd: Dir, env: List((String, String)))
    -> Result(Exit, String)

# RFC-0004's witchyc driver becomes:  spawn(witchyc, ["build", …], …)   witchyc: File[Exec]
```

**The caveat — read this before celebrating.** The capability model's confinement
**ends at the exec boundary.** A child process runs with full OS authority, *outside*
witchy's capabilities — witchy cannot sandbox what it spawns. So `File[Exec]` on a
shell, an interpreter, or a package manager is **effectively unbounded**, regardless of
being "one file": the binary's *behavior* is the real authority, not the file handle. So
`File[Exec]`:

- **does** improve the *spelling* — it names which binary, drops ambient "run anything,"
  and makes exec audit honestly;
- **does not** make exec *safe* — it remains the sharpest authority in the system.

Therefore `witchy caps` must surface the **exec'd path prominently** (e.g.
`File[Exec] /bin/sh  ⚠ unconfined child`), and review tooling should treat any
`File[Exec]` as the highest-scrutiny grant. We fold `Exec` into `File[Exec]` for
precision and auditability, with eyes open that the danger is intrinsic to exec, not to
the spelling.

### Footprint

`File[Read]`, `File[Write]`, `File[Exec]` appear in `witchy caps` like `Dir` rights; the
`File[Exec]` line is highlighted with its path and the unconfined-child warning.

## Alternatives

- **Keep `Dir`-only.** Over-grants for single-file needs and leaves the hierarchy
  lopsided (subtree but no leaf).
- **Keep ambient `Exec`.** Strictly worse than `File[Exec]`: no binary named, audits
  identically whatever it runs.
- **`File[Exec]` that also carries an arg/env policy** ("git, but only `status`").
  Deferred — a further *library* refinement (RFC-0011) over a `File[Exec]`, not part of
  the primitive. Worth a follow-up given how broad exec is.
- **A separate non-capability "path" type for files.** Rejected — a path the program can
  open at will is ambient filesystem authority; the handle must *be* the authority
  (RFC-0011).

## Drawbacks

- **More host capability types** (`File` × three rights) and `spawn` re-plumbed off a
  `File[Exec]`. Mitigated by `File` reusing `Dir`'s rights model wholesale.
- **The exec caveat is intrinsic.** `File[Exec]` cannot deliver the hard confinement
  `File[Read]` does; the model's guarantees genuinely stop at the process boundary, and
  no spelling fixes that.
- **Migration.** Existing `Exec` holders (RFC-0004's driver, any FFI/native path) move to
  `File[Exec]`; `--exec`-style grants become `--file-exec <path>`.

## Prior art

- [RFC-0011](./0011-capability-refinement.md) (capability refinement) — `File` is a host-primitive under its two-tier model;
  navigation vs. policy refinement.
- [RFC-0003](./0003-network-address-scoping.md) (Net scope-by-value) — the carried-state model `Dir`/`File` follow.
- [RFC-0004](./0004-self-hosted-cli.md) (self-hosted CLI) — the constrained, branded `Exec` that `File[Exec]`
  supersedes.
- [RFC-0002](./0002-user-definable-capabilities.md) (user-definable capabilities) — rights facets (`File[Read]` from
  `File[Read, Write]`).
