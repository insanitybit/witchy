# Sharing Code: Runes and the Registry

A language for running untrusted code has to take its *package manager*
seriously, because the package manager is where untrusted code comes from. The
npm/pip/cargo lineage all share a weakness: installing a dependency, and often
*building* it, runs with your full ambient authority. witchy's package manager,
**coven**, is built so that depending on code can't, by itself, become a new way
to be attacked.

## Runes

A witchy package is a **rune**: a directory with a `witchy.toml` manifest and a
`src/` tree. Scaffold one:

```sh
witchy new acme/shout
```

A library rune just exports `pub` functions:

```witchy
// src/shout.witchy
pub fn shout(s: String) -> String:
    "HEY " <> to_upper(s)
```

Notice it has no capability parameters — this rune is pure, and that fact is
about to become something a consumer can *verify*, not just trust.

## The footprint is recomputed, never trusted

The manifest can *declare* a capability footprint, but the registry and the
client don't believe it. When you publish, and again when anyone resolves a
dependency, the footprint is **recomputed from the source** — the same analysis
as `witchy caps`. Declared metadata that disagrees with the code is ignored in
favor of the code.

This is the load-bearing idea. In other ecosystems, "this package only needs
network access" is a claim in a README. In witchy it's a fact derived from the
source every time, so it can't drift, and it can't lie.

## Adding a dependency gates on widening

```sh
witchy add acme/shout
```

`add` fetches the rune, verifies its signature, and checks its footprint against
what you've approved. If a rune — or anything in its transitive tree — demands
authority you haven't accepted, the command **blocks** and tells you what new
power appeared:

```text
acme/shout@1.0.0 demands no capabilities.
tree max authority now: none
```

The real value shows up on *upgrades*. Suppose `acme/logger@1.0.0` is pure, you
depend on it, and `1.1.0` quietly adds a function that takes a `Net`:

```sh
witchy update
```

```text
blocked: acme/logger 1.0.0 -> 1.1.0 widens the footprint
  + Net
run `witchy update --allow-cap Net` to accept, or pin the old version
```

A dependency cannot silently start touching the network between versions. The
gate forces the new authority to be seen and accepted — a code-review signal
that's verb-precise (`Net[Listen]` is different from `Net[Connect]`) and
impossible to miss. `witchy audit` shows the whole tree's aggregate authority at
any time.

## Trusted publishing, two-phase release

Publishing doesn't use a long-lived API key that can leak. It uses short-lived
**identity tokens** — the same OIDC shape CI systems like GitHub Actions provide
— so a publish is bound to a specific repository and workflow. The first publish
to a namespace binds it; a token from any other repository is refused, which
shuts down namespace hijacking.

Release is two-phase. A publish lands **staged** — visible but not resolvable.
A separate **promote**, by a different identity and with a second factor, makes
it a real release. Separation of duties is enforced: the promoter can't be the
uploader. Registry metadata is signed (TUF-style) to resist rollback and
tampering, and lockfiles pin content hashes, the registry's key, and the full
provenance chain, all re-checkable offline with `witchy verify`.

Crucially, **dependency code never runs at build time.** Resolving and linking a
rune reads and type-checks its source; it does not execute it. The build step
can't be a vector, because there's nothing to exploit there.

## Try the whole thing locally

All of this runs on your machine — there's a scripted tour that starts a
registry, publishes through trusted publishing, promotes with a second factor,
consumes the rune from a separate project, and demonstrates the widening gate
refusing an over-reaching upgrade:

```sh
./scripts/local-registry-demo.sh
```

The repository's `docs/local-registry.md` walks through it step by step, and
`docs/package-manager.md` is the full design and threat model. The package
manager and the registry are themselves written in witchy (`projects/pm` and
`projects/coven`) — the language eats its own dog food, sandboxable footprint
and all.
