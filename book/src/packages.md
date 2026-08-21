# Sharing Code: Runes and the Registry

A language for running untrusted code has to take its *package manager*
seriously, because the package manager is where untrusted code comes from. The
npm/pip/cargo lineage all share a weakness: installing a dependency, and often
*building* it, runs with your full ambient authority. witchy's package manager,
**coven**, keeps dependency installation and build authority explicit, so adding
code enters through a reviewed capability change.

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
    "HEY ${s.to_upper()}"
```

Its recomputed root capability demand is empty, and this implementation is
effect-free as written. Lack of capability parameters alone is not a purity
contract for arbitrary higher-order code: an ordinary callback can carry
behavior deliberately delegated by its caller.

## Consume your first package

Runes are shared through a **registry**. There's a hosted one at
`https://witchy.fly.dev`; the client reads its address from the `COVEN_URL`
environment variable, and with none set it dials the local default
`127.0.0.1:8787` - so a brand-new user has to point it at the hosted registry
before anything resolves:

```sh
export COVEN_URL=https://witchy.fly.dev
witchy new demo-app && cd demo-app
witchy add insanitybit/hello --allow-fresh   # --allow-fresh accepts a release still inside its staging cooldown
```

Why `--allow-fresh`? Every promoted release sits out a **staging cooldown**
(72 hours by default) before `add` will resolve it - a window in which a
compromised release can be spotted before anyone installs it. On a young
registry every release is still inside that window, so the very first `add`
needs `--allow-fresh` to opt in explicitly. A release older than the cooldown
resolves with no flag at all; `--allow-fresh` is only the honest first step on a
fresh registry, not a permanent part of the workflow.

Now import the rune and use it:

```
// src/demo-app.witchy
import hello

fn main(console: Console):
    console.print(hello.greeting())   // whatever the rune exports; `witchy doc` lists it
```

```sh
witchy run .
witchy tree .    # the dependency, plus the capability footprint it pulls in
```

`witchy tree` prints the resolved dependency tree with each rune's recomputed
capability footprint alongside it, so before you trust `hello` you can see the
root authority it - and its transitive dependencies - demand. The rest of this
chapter is about that footprint: how it's computed, and how adding or upgrading
a dependency gates on it.

## The footprint is recomputed, never trusted

The manifest can *declare* a capability footprint, but the registry and the
client don't believe it. When you publish, and again when anyone resolves a
dependency, the footprint is **recomputed from the source** - the same analysis
as `witchy caps`. Declared metadata that disagrees with the code is ignored in
favor of the code.

In other ecosystems, "this package only needs network access" may be a README
claim. witchy derives the footprint from source each time, so a declared
metadata from drifting away from the code.

## Adding a dependency gates on widening

```sh
witchy add acme/shout
```

`add` fetches the rune, verifies its signature, and checks its footprint against
what you've approved. If a rune - or anything in its transitive tree - demands
authority you haven't accepted, the command **blocks** and tells you what new
power appeared:

```text
acme/shout@1.0.0 demands no capabilities.
tree max authority now: none
```

The real value shows up on *upgrades*. Suppose `acme/logger@1.0.0` has an empty
root footprint, you depend on it, and `1.1.0` quietly adds a function that takes
a `Net`:

```sh
witchy update
```

```text
blocked: acme/logger 1.0.0 -> 1.1.0 widens the footprint
  + Net
run `witchy update --allow-cap Net` to accept, or pin the old version
```

A dependency can't silently add a new root network demand between versions.
The gate forces that new authority to be seen and accepted - a code-review
signal that's verb-precise (`Net[Listen]` is different from `Net[Connect]`) and
impossible to miss. `witchy tree` shows the whole dependency tree's root demand
at any time - each rune's recorded footprint alongside it - and `witchy why-cap
<dir> <Cap>` traces which dependency pulls a given capability in.

That guarantee does not replace behavioral review. A dependency can change how
it invokes an ordinary callback that the application already delegated without
widening its capability footprint. The receiver still does not possess the
callback creator's captured capabilities; it possesses the narrower callable
interface. APIs that require effect-free plugin behavior express that with the
checked `pure fn` contract.

## Trusted publishing, two-phase release

Publishing uses short-lived **identity tokens** - the same OIDC shape CI systems
like GitHub Actions provide -
so a publish is bound to a specific repository and workflow. The first publish
to a namespace binds it; a token from any other repository is denied, which
shuts down namespace hijacking.

Release is two-phase. A publish lands **staged** - visible but not resolvable.
A separate **promote**, by a different identity and with a second factor, makes
it a real release. On a trusted registry, Coven accepts that proof only from the
verified identity token's issuer-signed `amr` claim (`mfa` or `webauthn`) and
consumes the token's `jti`; a request-body marker can't release anything.
Coven Web instead verifies a fresh passkey assertion at the web edge and may
forward to an internal anonymous-mode Coven that's never exposed directly.
Separation of duties is enforced: the promoter can't be the uploader. And even
once released, a version sits out a **staging cooldown**
(72 hours by default) before `add`/`update` will resolve it - time for a
compromised release to be noticed before anyone consumes it - unless you accept
it explicitly with `--allow-fresh`. The release timestamp is part of the signed
record, so the window can't be erased by tampering. Registry metadata is signed (TUF-style) to resist rollback and
tampering, and lockfiles pin content hashes, the registry's key, and the full
provenance chain, all re-checkable offline with `witchy verify`. Add `--online`
when you also want to re-fetch TUF metadata and check freshness or rollback.

**Resolving and installing a rune never executes its code**: it's
read and type-checked, nothing more. There's no `postinstall`, no `build.rs`
running with your ambient authority.

## Build steps are capabilities too

Some runes legitimately need to run code at *build* time - generating witchy
source from a schema, say. That is the one place code executes outside your
type-checked call graph, which makes it exactly where supply-chain attacks live
in other ecosystems. witchy models it with the same machinery as runtime: a rune
ships a `src/build.witchy` whose `fn build` entrypoint may take **only build
capabilities**; its build footprint is recomputed, recorded in your lockfile,
gated on widening, and runs only under per-rune grants you write in your own
`witchy.toml`. The [build-steps chapter](packages-build.md) walks the whole
flow with real tool output.

## Try the whole thing locally

The repository includes a scripted tour that starts a
registry, publishes through trusted publishing, promotes with a second factor,
consumes the rune from a separate project, and demonstrates the widening gate
rejecting an over-reaching upgrade:

```sh
./scripts/local-registry-demo.sh
```

The repository's [`spec/local-registry.md`](https://github.com/insanitybit/witchy/blob/master/spec/local-registry.md)
walks through it step by step, and [`rfcs/package-manager.md`](https://github.com/insanitybit/witchy/blob/master/rfcs/package-manager.md)
is the full design and threat model. The package manager and the registry are
themselves written in witchy ([`projects/grimoire`](https://github.com/insanitybit/witchy/tree/master/projects/grimoire)
and [`projects/coven`](https://github.com/insanitybit/witchy/tree/master/projects/coven)) -
the language eats its own dog food, sandboxable footprint and all.
