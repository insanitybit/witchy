# Deploying the coven registry on Fly.io

This runbook deploys the coven registry - the embedded registry server built
into the `witchy` toolchain binary (`witchy coven-serve`) - as a single
TLS-fronted machine on Fly.io. It assumes no knowledge of the coven source.

Artifacts: [`deploy/Dockerfile`](deploy/Dockerfile),
[`deploy/entrypoint.sh`](deploy/entrypoint.sh),
[`deploy/fly.toml`](deploy/fly.toml). RFC: `rfcs/0116-hosted-coven-registry-m1.md`.

## What you are deploying

`witchy coven-serve` is a capability-confined HTTP server. Inside the
container the entrypoint runs (after materializing the signing seed and
dropping root):

```sh
witchy coven-serve --addr 0.0.0.0:8080 --root /data --signing-key <tmpfs seed file>
```

- `--addr <host:port>` - the listen address (a flag, not a positional).
- `--root <dir>` - the registry store, a plain directory tree. Here it is the
  Fly volume mounted at `/data`; the store IS the registry state, there is no
  database.
- `--signing-key <file>` - path to a file holding 64 hex characters, the
  registry's 32-byte Ed25519 signing seed. (`--secret-file signing=<path>` is
  the equivalent general form.)
- `--trust-issuer <issuer>=<pubkeyhex>` and
  `--trust-issuer-jwks <issuer>=<jwks-file>` - optional, repeatable trusted
  publishing issuers (see "Configuring issuer trust").

Anything else (including positional arguments) is rejected: those are all of
`coven-serve`'s flags.
Capability grants (network listen, the store directory, the signing secret)
are wired by the toolchain itself; you never pass `--net`/`--dir` here.

Routes are all under `/coven/…`; `GET /coven/rootpub` (returns the signing
public key, HTTP 200 text) is the health probe, and `GET /coven/index` lists
package names.

## Prerequisites

- `flyctl` installed and authenticated (`fly auth login`), with an
  organization that can create apps, volumes, and secrets.
- `openssl` locally, for seed generation.
- A checkout of this repository - the image builds the toolchain from source,
  so `fly deploy` runs from the REPO ROOT with the workspace as build context.
- Docker is NOT required locally (Fly builds remotely by default), but
  `docker build -f projects/coven/deploy/Dockerfile .` from the repo root is a
  useful preflight.

## 1. Signing-seed ceremony

The seed is the registry's ONLY signing identity (single root key; delegated
TUF keys are explicitly deferred by RFC-0116). Every published record, the
TUF `snapshot.json`, and `timestamp.json` are signed with it, and every pm
client pins the derived public key (`registry_rootpub` in its `witchy.lock`,
trust-on-first-use). Treat it accordingly.

Generate it OFFLINE, on the operator's machine, and store it ONLY as a Fly
secret:

```sh
fly secrets set --app witchy-coven COVEN_SIGNING_SEED=$(openssl rand -hex 32) --stage
```

Rules:

- Never commit the seed, never bake it into the image, never echo it into
  shell history you keep (the command above expands it inline; use
  `HISTFILE=` or a throwaway shell if that concerns you).
- The value must be exactly 64 hex characters (32 bytes); both the entrypoint
  and `witchy` validate this and refuse to start otherwise.
- Mechanism: Fly delivers the secret to the machine as the
  `COVEN_SIGNING_SEED` environment variable. The container entrypoint writes
  it to a tmpfs file (`/dev/shm/coven-signing.seed`, mode 0400, owned by the
  unprivileged runtime user), unsets the variable, and passes the file path to
  `--signing-key`. The seed never touches the volume or the image.
- Optionally keep a sealed offline copy (paper or an encrypted vault) if you
  want registry identity to survive a Fly-account loss. If you keep no copy,
  the Fly secret is the single point of identity.

### Loss and rotation

There is no key-rotation machinery in M1. Understand the blast radius before
you need it:

- **Loss** (secret gone, no offline copy): the registry can no longer sign
  new records or refresh `snapshot.json`/`timestamp.json`. Clients verify
  timestamp freshness, so the registry goes stale and then unusable. Recovery
  is a new seed, which is the rotation case below.
- **Rotation** (`fly secrets set COVEN_SIGNING_SEED=…` with a new value, then
  redeploy/restart): the public key changes, so this is effectively a NEW
  registry identity. Every pm client's pinned `registry_rootpub` now
  mismatches and pm BLOCKS (by design - a changed root key is
  indistinguishable from a registry compromise). Records signed under the old
  key no longer verify, and there is no re-signing tool. Practically,
  rotation means: re-publish the registry contents under the new key, and
  every client deliberately deletes its pin (edit `witchy.lock`) and
  re-establishes trust out of band. Announce the new public key
  (`/coven/rootpub`) over a channel clients already trust.

## 2. Create the app and volume

```sh
fly apps create witchy-coven          # pick your own unique name
fly volumes create coven_data --app witchy-coven --region iad --size 1
```

Update `app` (and `primary_region` if not `iad`) in `deploy/fly.toml` to match.
The store is a directory tree, not a replicated database: run ONE machine with
ONE volume. Do not scale out.

## 3. Deploy

From the repository root (the Docker build context must be the workspace -
the builder compiles `witchy` from source with `cargo build --release
--locked`, and the coven + stdlib sources are embedded in the binary):

```sh
fly deploy . --config projects/coven/deploy/fly.toml \
             --dockerfile projects/coven/deploy/Dockerfile
```

First build compiles the whole Rust workspace; expect tens of minutes on
Fly's remote builder. Subsequent deploys reuse layer cache when the source
layer is unchanged.

Verify:

```sh
fly status --app witchy-coven
fly logs --app witchy-coven
```

The health check probes `GET /coven/rootpub` (also the Dockerfile
HEALTHCHECK for plain-Docker runs). From your machine:

```sh
open https://witchy-coven.fly.dev/coven/rootpub   # 64-hex public key
open https://witchy-coven.fly.dev/coven/index     # {"names":[...]}
```

Record the `/coven/rootpub` value in your ops notes: it is the public
identity clients pin, and your check during any incident that the registry
still holds the right key.

## 4. Configuring issuer trust (trusted publishing)

With no issuers configured the registry is ANONYMOUS (local-mode publishing;
fine for a private experiment, wrong for anything shared). Trusted publishing
binds namespaces to a CI identity from an OIDC issuer.

M1 supports the two offline trust forms only - a pinned Ed25519 issuer key,
or an inline JWKS document. **Live JWKS-over-HTTPS discovery
(`--trust-issuer-oidc <issuer-url>`) is RFC-0116 track 2 and does NOT exist
yet** - to trust a real IdP (e.g. GitHub Actions) today you hand-copy its JWKS.

Set specs via the `COVEN_TRUST_ISSUERS` env var, one `--trust-issuer` value
per line:

```sh
# Pinned issuer key:
fly secrets set --app witchy-coven \
  COVEN_TRUST_ISSUERS='https://issuer.example=6fe4…hex…'

# Inline JWKS (compact the JSON to one line first, e.g. jq -c):
fly secrets set --app witchy-coven \
  COVEN_TRUST_ISSUERS='https://token.actions.githubusercontent.com=jwks:{"keys":[…]}'
```

(Env config rather than baked args so trust changes are a restart, not an
image rebuild. `fly secrets set` triggers the restart itself. Because the
JWKS is a static copy, upstream key ROTATION requires you to re-fetch and
re-set it - until track 2 lands, check it whenever CI publishes start failing
verification.)

## 5. Backups and the restore drill

Everything lives on the `coven_data` volume. Fly takes automatic daily
snapshots (default retention 5 days); take a manual one before anything
risky:

```sh
fly volumes list --app witchy-coven                 # note the volume id
fly volumes snapshots list <volume-id>
fly volumes snapshots create <volume-id>            # manual snapshot
```

**Restore drill** - run this once BEFORE you rely on backups, and again after
any material change:

1. `fly volumes snapshots list <volume-id>` - pick a snapshot id.
2. Create a fresh volume from it:
   `fly volumes create coven_data_restore --app witchy-coven --snapshot-id <snapshot-id> --region iad`
3. Point a machine at it: either edit `[mounts].source` to
   `coven_data_restore` in a scratch copy of `fly.toml` and deploy a staging
   app, or `fly machine clone` with the restored volume attached.
4. Verify: `/coven/rootpub` matches your recorded key, `/coven/index` lists
   the expected names, and a client `witchy add` of a known package succeeds.
5. Destroy the drill resources when done (`fly volumes destroy …`).

The signing seed is NOT on the volume (it exists only as the Fly secret), so
a volume restore alone never rotates the key. Losing BOTH the app (secrets)
and the volume loses the registry; see the ceremony section.

## 6. Logs and health

```sh
fly logs --app witchy-coven            # live server log (one line per request)
fly status --app witchy-coven          # machine + health-check state
fly checks list --app witchy-coven     # the /coven/rootpub probe
```

Known quirk (verified in-container): the server writes its readiness line
(`coven serving at http://0.0.0.0:8080  (root key <64-hex pubkey>; ...)`)
WITHOUT a trailing newline, and newline-framed log collectors (`fly logs`,
`docker logs`) hold an unterminated line forever - so a HEALTHY registry can
show an empty log stream. Treat the health check and an HTTPS probe of
`/coven/rootpub` as the operational truth, not log volume. Entrypoint and
startup FAILURES do log normally: `COVEN_SIGNING_SEED is not set` / `must be
exactly 64 hex characters` means the secret is missing or malformed, and a
crash loop right after `fly secrets set` usually means an invalid
`COVEN_TRUST_ISSUERS` spec.

Expect the first boot after a deploy to take ~15-30 seconds before the port
opens (the toolchain compiles the embedded server to wasm once, then caches
it); the health check's grace period covers this.

## Client side (read before announcing a URL)

`COVEN_URL=https://witchy-coven.fly.dev` does NOT work with the shipped pm
client yet: pm currently discards the URL scheme and rebuilds the origin as
`http://` (RFC-0116 track 1, scheme-aware addressing, fixes this). Until
track 1 lands, only loopback `http://host:port` registries are reachable by
`witchy pm`/`witchy add`. Deploying ahead of that is still useful - the
registry is exercisable with any HTTPS client and ready the moment the client
lands - but do not hand the URL to users expecting `witchy add` to work.

## What this deployment does NOT promise

`coven-serve` is **Experimental** (see `PRODUCT-STATUS.md`): it is dogfood
for Witchy and the package protocol. This deployment therefore carries:

- **No availability promise** - one machine, one region, no failover.
- **No durability promise** - one volume plus snapshots; snapshots are
  point-in-time and can lose the most recent publishes.
- **No security-review promise** - the server has NOT had an independent
  security review; the trust machinery (OIDC-bound publishing, TUF
  snapshot/timestamp, capability-widening gates) is implemented and tested
  in-tree, but running it on the public internet is at your own risk.
- **No hosted-service promise** - this runbook makes YOUR deployment, not an
  official witchy registry.
