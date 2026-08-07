# Running the registry locally

The whole package pipeline - registry server, trusted publishing, two-phase
release, signature-verified consumption, capability auditing - runs on
localhost with nothing but the `witchy` binary. One command tours all of it:

```sh
cargo build --release
./scripts/local-registry-demo.sh
```

The rest of this page is the same flow by hand, so you can keep a registry
running and use it across your own projects.

## 0. Isolation (optional but recommended)

Everything client-side (the content-addressed store, your signing identity)
lives under `WITCHY_HOME` (default `~/.witchy`). Point it somewhere fresh to
experiment without touching your real store:

```sh
export WITCHY_HOME=~/witchy-playground
```

## 1. Start a registry

```sh
mkdir -p ~/coven-data
witchy coven-serve --addr 127.0.0.1:8470 --root ~/coven-data
```

The server stores everything under `--root` as content-addressed, signed
records. Point clients at it:

```sh
export COVEN_URL=http://127.0.0.1:8470
```

That's a usable registry already - publishes are signed by the registry key
and clients verify on fetch. For the full **trusted publishing** flow (no
long-lived credentials; publishers present short-lived identity tokens, the
way GitHub Actions OIDC works), also generate a demo identity provider and
tell the server to trust it:

```sh
witchy coven-gen-issuer --out ~/coven-idp        # prints the issuer pubkey
witchy coven-serve --addr 127.0.0.1:8470 --root ~/coven-data \
    --trust-issuer "local-idp=<that pubkey hex>"
```

## 2. Publish a rune

A rune is a directory with a manifest and sources:

```
shout/
  witchy.toml          [rune] name = "acme/shout"  version = "1.0.0"
  src/shout.witchy     pub fn shout(s: String) -> String: ...
```

Mint a CI identity token and publish with it:

```sh
TOKEN=$(witchy coven-mint-token --issuer-key ~/coven-idp --issuer local-idp \
    --sub "repo:acme/shout-repo:ref:refs/heads/main" \
    --claim repository=acme/shout-repo --claim workflow_ref=release.yml \
    --claim ref=refs/heads/main)
cd shout && COVEN_ID_TOKEN=$TOKEN witchy publish
```

Three things happen, none of them on trust:

- the registry **recomputes the rune's capability footprint from source**
  (declared metadata is ignored);
- the first publish to a namespace **binds it to that repository + workflow** -
  a valid token from any other repo is refused thereafter;
- the version lands **STAGED**: visible, but not resolvable by anyone.

## 3. Promote (release)

A *different* identity (separation of duties is enforced) releases it with a
second factor:

```sh
HUMAN=$(witchy coven-mint-token --issuer-key ~/coven-idp --issuer local-idp \
    --sub alice --claim amr=webauthn)
COVEN_ID_TOKEN=$HUMAN witchy promote acme/shout 1.0.0
```

The local IdP helper models a provider that attests the human's authentication
method. Coven verifies the token signature and reads `amr` from those verified
claims; the promote request itself cannot assert its own second-factor proof.

## 4. Consume it from another project

```sh
mkdir -p app/src && cd app
cat > witchy.toml <<'EOF'
[rune]
name = "demo/app"
version = "0.1.0"
EOF
witchy add acme/shout       # fetches over HTTP, verifies, and GATES:
                            # this blocks if the footprint demands authority
                            # you haven't approved
witchy tree                 # the dependency tree, annotated with capabilities
witchy audit                # provenance + aggregate authority of the tree
witchy run
```

The lockfile pins the content hash, the registry's signing key fingerprint,
and the full trusted-publishing provenance chain - `witchy verify` re-checks
all of it offline, while `witchy verify --online` additionally refreshes TUF
metadata to check freshness and rollback. `witchy vendor` materializes the
sources if you want zero registry dependence at build time.

## 5. Watch the gates work

- Bump the version, mint a token claiming a **different repository**, and
  `publish` - refused (namespace binding).
- Add a capability parameter (say `net: Net`) to a published function and
  release the new version - `witchy update` / `witchy outdated` flag the
  **footprint widening** and block until you approve it explicitly.
- Try `witchy add` against a STAGED version - refused until promoted.

## What this is and isn't

This is the real lifecycle, end to end, on your machine - the same code paths
the e2e suite drives. What separates it from a production deployment is
operational, not functional: TLS termination in front of the server, a real
OIDC issuer and live JWKS discovery (the demo issuer and pinned-key/JWKS inputs
stand in for it), a TUF root-key ceremony, and backups. See
[package-manager.md](../rfcs/package-manager.md) §15 for the status table.
