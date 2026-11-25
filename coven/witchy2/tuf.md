# Coven releases over TUF

The TUF path takes a witnessed private candidate through a delegated targets
signer to a standard static repository. Grimoire verifies both the TUF chain
and the release evidence before any source reaches the compiler. An ordinary
TUF client downloads the same target and verifies the TUF chain.

Install the pinned Python environment using [the approval setup](approval.md).
Keep the staging store and release signing machines separate from the public
mirror. Provision the initial TUF root and release trust file independently.

## Approve and seal a candidate

Pack the package and follow the private WebAuthn ceremony in [Approval](approval.md).
The release trust JSON contains these fields:

| Field | Value |
| --- | --- |
| `format` | `coven-target-1-ml-dsa-87` |
| `policy` | The complete enrolled WebAuthn policy for this registry |
| `publishers` | A map from package name to its ML-DSA-87 public key |
| `witness` | The independent ML-DSA-87 approval witness public key |
| `policy_history` | Optional older policies paired with SHA-384 pins of the exact sealed bundles they can verify |

Keys use unpadded base64url of the raw public key bytes. Use independent
32-byte ML-DSA-87 seed files with owner-only access for the publisher,
witness, and delegated TUF signer. The format uses SHA-384 for package and
evidence integrity. The publisher and witness sign in a Witchy guest.
The host grants each signer only its own message domain through an inherited
descriptor. The witness checks the active policy, publisher signature, and
WebAuthn assertion before signing. A FIPS deployment requires separate
validation of the module and environment.

```sh
coven tuf sign-package candidate.coven --registry-id my-registry \
  --key publisher.seed --out publisher.sig
coven tuf seal "$digest" --staging-dir ./staging --publisher-signature publisher.sig \
  --trust release-trust.json --witness-key witness.seed --out approved.coven
```

The publisher worker reads and validates the packed package and writes a
detached signature with only the publisher signing grant. A Witchy worker
with read-only access to the candidate, policy and approval tables selects
all three in one transaction. The witness worker receives that evidence,
its own signing grant and a clock. The launcher reserves the database for
the duration of sealing so policy changes cannot race the publication of
the signed target. Deploy the matching host for the read-only worker.

Seal while the approval intent is still valid. The witness signs the release
coordinate, digest, publisher signature, WebAuthn evidence, and observation
time. The Witchy worker reads that time from its granted clock and emits the
complete canonical sealed bundle. This lets a consumer check when the approval
was witnessed after the intent has expired. The verifier compares the receipt
with the provisioned policy, including registry, credential scope, origin,
and user verification.

### Sign and seal without a compiler

Build and check the workers on the trusted build machine. Provision the
Wasm artifacts and their independent SHA-384 pins on the machines holding
the publisher and witness keys. The [Linux release archive](../deploy/witchy2/README.md#provision-the-release-workers)
contains these workers and their pins. Wasmtime verifies the pin against
the module bytes it loads and stops the worker if the launcher exits.

```sh
src=coven/witchy2
for worker in tuf_publisher_sign tuf_witness_seal tuf_seal_snapshot; do
  witchyc wasm-caps --policy "$src/$worker.policy" "$src/$worker.wy"
  case "$worker" in
    tuf_publisher_sign) heap=96 ;;
    tuf_witness_seal) heap=256 ;;
    tuf_seal_snapshot) heap=64 ;;
  esac
  witchyc build "$src/$worker.wy" -o "$worker.wasm" \
    --heap-pages="$heap" --slots=4096 --stack-bytes=65536
done
```

```sh
coven tuf sign-package candidate.coven --registry-id my-registry \
  --module "$publisher_module" --module-pin "$publisher_pin" \
  --key publisher.seed --out publisher.sig
coven tuf seal "$digest" --staging-dir ./staging --publisher-signature publisher.sig \
  --module "$witness_module" --module-pin "$witness_pin" \
  --snapshot-module "$snapshot_module" --snapshot-pin "$snapshot_pin" \
  --trust release-trust.json --witness-key witness.seed --out approved.coven
```

Pinned sealing requires both worker and pin pairs. The snapshot worker gets
read-only database access. The witness worker gets a signing capability
bound to its message domain.

## Rotate the maintainer policy

After installing a new staging policy, give `rotate-policy` the new policy
JSON and the published sealed bundles that should remain available. Use an
independently authenticated copy of the previous release trust and its pin.

```sh
coven tuf rotate-policy published/approved.coven \
  --trust release-trust.json --trust-pin "$trust_pin" \
  --policy new-policy.json --out rotated-trust.json
```

The read-only Witchy worker checks the previous trust pin, verifies each newly
retained bundle, and pins its exact sealed bytes. It carries forward existing
archive entries and requires a higher policy revision for the same registry
and origin before emitting the canonical trust file. Provision
the resulting trust file and its printed pin through the existing operator
channel before advertising releases approved under the new policy.

Fresh releases use the active policy. For a retained release, both the TUF
publisher and Coven fetch require the sealed bundle pin and recheck its
publisher, witness, and WebAuthn evidence under the referenced policy. TUF
metadata continues to authenticate the exact delivered target bytes.
[TUF target verification](https://theupdateframework.github.io/specification/latest/#fetch-target)

An archive entry contains `policy` and a nonempty `targets` array of up to
2,048 SHA-384 digest strings. The trust file supports up to 16 historical
revisions within its 256 KiB limit. `rotate-policy` preserves the publisher
and witness keys. Treat rotation of those authorities as a separate operation.

## Authorize target metadata

Supply the previously authenticated parent Targets metadata and its SHA-384
pin. A read-only Witchy worker verifies each bundle, derives the target path
from its approved coordinate, and writes unsigned stock TUF Targets metadata
with expiry derived from its clock. Preparation and signing read the same
private copy of the selected bundles. The Witchy signer checks the parent and
release trust pins, the `NAME/*` scopes of the selected role, the expiry,
and the key's assignment and independence. It opens the sealed bundle for each
target and verifies its length, digest, coordinate, and evidence in the invocation
that signs the metadata. Each seed reaches the host through a separate
descriptor carrying only TUF Targets signing authority. A separate Witchy
worker verifies the signature threshold against the pinned parent and writes
the signed envelope without access to signing keys.
`--key` may be repeated to meet the role threshold. The caller sets the next
version and supplies all bundles that will remain active in the role.
[TAP 21](https://github.com/theupdateframework/taps/blob/master/tap21.md)

```sh
coven tuf authorize approved.coven --role my-package \
  --parent 1.targets.json --parent-pin "$parent_pin" \
  --trust release-trust.json --trust-pin "$trust_pin" \
  --key delegated.seed --version 2 --valid-days 7 --out 2.my-package.json
```

## Publish a signed update

Use independently held roles to sign top-level targets, snapshot, and timestamp
metadata. The initial root requires a threshold of at least two keys. Supply
rotations as consecutive `N.root.json` files signed by both the old and new
thresholds. The mirror publisher takes public verification inputs only.

Prepare an incoming directory separate from the mirror. Place the signed
timestamp at `metadata/timestamp.json`. Place supporting metadata at
`metadata/N.ROLE.json` and sealed bundles at
`targets/NAME/SHA384.VERSION.coven`, where `SHA384` is the digest of the entire
sealed bundle. Files already published can be omitted from the incoming
directory. The publisher takes lengths and SHA-384 hashes from the signed
metadata. [TUF consistent snapshots](https://theupdateframework.github.io/specification/latest/#consistent-snapshots)

```sh
mkdir -m 755 ./mirror
coven tuf publish --incoming ./incoming --mirror ./mirror \
  --root registry-root.json --root-pin "$root_pin" \
  --trust release-trust.json --trust-pin "$trust_pin"
```

The installed Witchy Wasm host runs the publisher's checks. Use the
[pinned module](#authorize-rotate-and-publish-without-a-compiler) on a
machine without the compiler. Publication uses public inputs and
requires no Python TUF or crypto packages. Witchy checks the root chain, each signature threshold, metadata
expiry and rollback, package delegation scopes, and independent approval
for every active release. It verifies ES256 WebAuthn assertions with no
authenticator extensions. Top-level targets delegates disjoint `NAME/*`
namespaces. Package roles list bundles directly and use distinct ML-DSA-87
keys. Withdraw a package by publishing an empty role, or withdraw one release
by omitting it from the next version of its role.

The publisher holds an exclusive lock on the operator-owned mirror from
the rollback check through commit. It stages verified bytes privately,
keeps immutable targets and versioned metadata in place, syncs each new
file and its directory, then replaces `timestamp.json` atomically. An interrupted update can be retried
with the same signed inputs. The mirror retains the older immutable files for
clients holding an older snapshot and preserves a version's original bundle
digest after withdrawal. When rotating an online role key, plan for
clients that refresh during the brief interval between the root and timestamp
updates. If the publisher launcher exits during preparation, the Wasmtime
lifeline stops the worker and releases its copy of the mirror lock.

The [Witchy mirror](mirror/README.md) serves this layout for local verification.
Public service requires HTTPS.

Serve only the mirror directory through a read-only HTTPS static web server.
Disable directory listing, keep the timestamp uncached, and use a separate
service account for the static server. The incoming directory, staging store,
and signing keys belong outside the served tree.

### Authorize, rotate, and publish without a compiler

Build workers on the trusted build machine. Deliver each Wasm file and
its SHA-384 pin through independent provisioning. Use the same command
inputs shown above, with these worker flags.

| Command | Worker | Arguments |
| --- | --- | --- |
| `tuf rotate-policy` | `tuf_rotate_policy.wasm` | `--module FILE --module-pin PIN` |
| `tuf authorize` | `tuf_delegate_prepare.wasm` | `--prepare-module FILE --prepare-pin PIN` |
| `tuf authorize` | `tuf_delegate_sign.wasm` | `--signer-module FILE --signer-pin PIN` |
| `tuf authorize` | `tuf_delegate_finalize.wasm` | `--finalizer-module FILE --finalizer-pin PIN` |
| `tuf publish` | `tuf_prepare_repository.wasm` | `--module FILE --module-pin PIN` |

Pinned authorization requires all three pairs. The signer has access to
exactly one delegated signing key per invocation. The preparer and
finalizer have no signing grants. The publication worker reads signed
inputs and the current mirror, and can write only to the private spool.
Wasmtime verifies the delegation and rotation module pins before
executing the workers. Each worker also follows its launcher's lifetime.
For Linux operators, the [publisher archive](../deploy/witchy2/README.md#publish-from-an-operator-machine)
ships the launcher, Wasmtime host, and publication worker with its pin.

```sh
src=coven/witchy2
for worker in tuf_rotate_policy tuf_delegate_prepare tuf_delegate_sign tuf_delegate_finalize tuf_prepare_repository; do
  witchyc wasm-caps --policy "$src/$worker.policy" "$src/$worker.wy"
  case "$worker" in
    tuf_rotate_policy) heap=256 slots=65536 ;;
    tuf_delegate_sign) heap=448 slots=4096 ;;
    tuf_delegate_prepare|tuf_delegate_finalize) heap=448 slots=65536 ;;
    tuf_prepare_repository) heap=256 slots=4096 ;;
  esac
  witchyc build "$src/$worker.wy" -o "$worker.wasm" \
    --heap-pages="$heap" --slots="$slots" --stack-bytes=65536
done
```

## Consume releases through Grimoire

[Install the client toolchain and proof checker](../../grimoire/docs/coven-http.md#install-the-pre-release-tools)
on the consumer machine. Obtain the initial TUF root, release trust document,
and their SHA-384 pins independently of the mirror. Use the canonical HTTPS
origin, with an explicit port only when it differs from 443. The origin
spelling identifies the client's rollback cache.

`coven tuf fetch` uses Witchy for HTTPS and TUF metadata refresh. A read-only
Witchy worker verifies each root rotation from the provisioned pin. Coven
persists each verified root before requesting its successor, so a failed
successor request leaves the latest trusted root in place. The worker
authenticates timestamp, snapshot, and Targets metadata before requesting
the next versioned object. Coven commits the authenticated timestamp
before fetching its snapshot and commits the authenticated snapshot
before fetching Targets metadata. The rollback floors survive an interrupted
refresh. Each checkpoint is written atomically relative to the locked
cache directory. A client refuses to complete verification if the cache
name no longer identifies the directory it locked.
Witchy checks that a later root chain passes through the exact root
recorded with the authenticated snapshot. It selects the exact
delegated target. A command waits up to 30 seconds for another command
holding that origin and root's cache lock. It reports busy state if the lock
is still held.

During incident recovery, Witchy checks the old and new root thresholds on
every root transition. A transition starts fresh timestamp and snapshot
floors when the previous timestamp keys or the previous snapshot keys can
no longer meet the applicable new threshold. Witchy compares the raw public
key fingerprints. Adding a key while the old keys still meet the threshold
keeps the floors. Checkpoints written under the replacement authority remain
in force across retries. Grimoire's existing project lock still requires
its pinned snapshot floor and all exact package pins.

Coven passes the authenticated mirror path and length
to the Witchy downloader. The download worker enforces that length while
receiving the target and writes it to a private file for extraction. It
receives one network endpoint,
a read-only PEM CA bundle, and a private response file. It authenticates
the configured DNS hostname and requests one same-origin path. The worker
enforces a maximum of 512,000 bytes for a root update and 16,384 bytes for a
timestamp as it reads the response, before publishing the private file.
Objects referenced by signed metadata still require the exact declared
length, up to 2 MiB. The host file budget covers the bound for that
request, the bytes in the staged CA file for HTTPS, and the cost of
publishing one file. Use HTTP/1.1 responses with `Content-Length` or a TLS
`close_notify` to delimit the body. An explicit IPv4 loopback HTTP origin
is available for test fixtures.
The TUF download, selection, extraction, signing, and review baseline
workers read an LF-terminated JSON header of at most 1 KiB. Workers that
receive binary data after the header separately check its declared length.
A header refusal stops processing before the worker reads the body.

Each refresh retrieves a fresh timestamp. Coven can reuse a saved snapshot,
top-level Targets file, or delegated role when its authenticated version
reference matches the saved file's length and digest. Witchy verifies those
bytes again before selection.
Pass `--expect-snapshot sha384:DIGEST` to require the exact snapshot
selected earlier in a multi-package operation. If fresh metadata selects
another snapshot, the command refuses before downloading a target. Supply
`--expect-target sha384:DIGEST` to pin the complete sealed target and
`--expect sha384:DIGEST` to pin the extracted release payload. Successful
fetches report the verified snapshot digest and version, target digest,
payload digest, and publisher key fingerprint.
[HTTP framing](https://www.rfc-editor.org/rfc/rfc9112.html#section-6.3) · [TLS closure](https://www.rfc-editor.org/rfc/rfc8446.html#section-6.1)

### Fetch and discover releases without a compiler

Build and check the client workers on a trusted build machine. Provision
the artifacts and their independent SHA-384 pins along with the TUF root
and release trust inputs.

| Worker | Arguments | Required for |
| --- | --- | --- |
| `tuf_fetch_select.wasm` | `--selector-module FILE --selector-pin PIN` | Fetch and catalog |
| `tuf_get.wasm` | `--download-module FILE --download-pin PIN` | HTTPS fetch and catalog |
| `tuf_get_loopback.wasm` | `--download-module FILE --download-pin PIN` | Fetch and catalog on an explicit IPv4 loopback HTTP test port |
| `tuf_extract.wasm` | `--extractor-module FILE --extractor-pin PIN` | Fetch |

When supplying pinned workers, pass every pair required for the command.
On each invocation, the Wasmtime host reads a bounded regular module file
and verifies its digest before executing the worker. A catalog invocation
has no extraction worker. Supply
the download worker for the chosen transport. The downloader, selector,
and extractor retain their own deadlines. Each also holds the read end
of a launcher lifeline. If Coven exits, the host stops the worker and
closes its open resources, including any active download connection.
Grimoire passes a lifeline to Coven for TUF fetches and catalog requests,
so stopping Grimoire also stops the active Coven command. Upgrade separately
pinned download modules and their SHA-384 pins with the launcher so the
worker accepts root and timestamp byte ceilings. Upgrade the extractor and
its pin with the launcher as well. The selector and its pin must match
the launcher's metadata checkpoint protocol. Rebuild an installed client
bundle with the matching launcher and selector. The extractor reads the
pinned trust document through bounded standard input. Its only filesystem grants are
the downloaded target and one staged package output, with a filesystem
byte budget based on the target size and the package size limit. The
loopback worker gets only the specified `127.0.0.1:PORT` endpoint and
one atomic output file. Its runtime deadline covers the entire exchange
and it verifies HTTP framing and the expected response length before
committing bytes. TUF still verifies metadata and package digests using
the same trust roots on both transports.

For deployment checks, `coven tuf catalog` accepts `--expect-snapshot`
and `--expect-timestamp`. Supply SHA-384 digests from the trusted
publication channel. The command authenticates current metadata and
refuses to print a catalog when either supplied digest differs.

Run `python3 coven/witchy2/tests/tuf_loopback_witness.py` from the
monorepo root to check the loopback worker against a local HTTP peer.

```sh
src="$prefix/libexec/coven"
for worker in tuf_fetch_select tuf_get tuf_get_loopback tuf_extract; do
  witchyc wasm-caps --policy "$src/$worker.policy" "$src/$worker.wy"
  case "$worker" in
    tuf_fetch_select) heap=448 slots=65536 ;;
    tuf_get) heap=48 slots=4096 ;;
    tuf_get_loopback) heap=40 slots=512 ;;
    tuf_extract) heap=256 slots=4096 ;;
  esac
  witchyc build "$src/$worker.wy" -o "$worker.wasm" \
    --heap-pages="$heap" --slots="$slots" --stack-bytes=65536
done
```

These four modules can also be included in the toolchain's
[pinned client ZIP](../../grimoire/docs/coven-http.md#install-from-a-pinned-client-bundle)
with `grimoire/install.py --make-bundle ... --tuf-workers WORKER_DIRECTORY`.
An installed client uses those pinned workers by default. The explicit
module flags still accept a complete independently pinned set. A worker
whose bytes differ from its pin is refused.

### Discover published versions

`coven tuf catalog` uses the same mirror, pins, and local rollback state as
`coven tuf fetch`. It emits the registry ID, the verified timestamp digest,
the snapshot version and digest, and every release coordinate from that
snapshot in one JSON object. Each entry
includes the length and SHA-384 of the sealed TUF target. Use the coordinate
with `coven tuf fetch` or in a Grimoire manifest to obtain the package source.

```sh
coven tuf catalog --registry "$mirror_url" --id "$registry_id" \
  --root "$root_file" --root-pin "$root_pin" \
  --trust "$trust_file" --trust-pin "$trust_pin"
```

Witchy checks all delegated metadata before Coven prints the list. The command
fetches metadata only, so the result is a view of advertised releases. Fetch
checks package availability, release attestations, and any locked source
digest separately. The catalog refuses snapshots exceeding 64 delegated
roles, 32 MiB of delegated metadata, or 4,096 releases.

The Witchy extraction worker receives the selected target and release trust file,
the authenticated target digest, and the requested registry and coordinate.
It uses the publisher's release-evidence verifier for the package, publisher
signature, witness signature, and maintainer assertion. It writes the exact
source bytes only after checking that evidence and any locked source digest.
Its grants cover the private input directory, private output file, clock,
and memory. [TUF target verification](https://theupdateframework.github.io/specification/latest/#fetch-target)

```toml
[registry]
protocol = "tuf"
url = "https://packages.example.com"
id = "my-registry"
root = "registry-root.json"
root-pin = "sha384:<96 lowercase hex digits>"
trust = "release-trust.json"
trust-pin = "sha384:<96 lowercase hex digits>"

[dependencies]
my-package = "1.0.0"
```

Put the provisioned files inside the project. Run `grimoire lock --toolchain`
and `grimoire review` with the installed toolchain on `PATH`. Review the
package source and capability policy. Use `grimoire review --write-policy`
to write a missing policy or `grimoire review --accept` to update the
digest pins in an existing policy. Then run `grimoire build` and
`grimoire run`. The downloader uses the OpenSSL default PEM CA file
selected by the launcher, including its `SSL_CERT_FILE` override when set.
For a project-specific CA, add `ca-file = "registry-ca.pem"` under
`[registry]` or give `--ca-file registry-ca.pem` to `coven tuf fetch`.
Provision the bundle's SHA-384 pin independently and set `ca-pin` in
the manifest or pass `--ca-pin` to Coven. Set `ca-pin` by itself to pin
the default PEM bundle without naming its path. A different PEM bundle
is refused even if it contains the same trusted roots. When no pin is
configured, Coven computes a SHA-384 pin for the selected PEM bytes. The Witchy
HTTPS worker checks that pin before it decodes the roots or opens a
connection. A successful HTTPS fetch receipt includes the effective
`ca_pin`.
The pin also applies to `coven tuf catalog` and `coven web init-tuf`. The web
scaffold copies the checked bundle and carries its pin into both
consumer manifests. An explicit file takes precedence over the
platform bundle. Grimoire keeps these trust files outside directories
the program can write. The recorded manifest digest covers the HTTPS
CA pin. The lock records the mirror origin, registry ID, TUF root,
release-trust pin, and the verified CA pin when the manifest supplies one.
It also records the exact sealed target and release
payload digests and publisher fingerprint for each dependency. Each
resolution requires a single verified TUF snapshot and HTTPS CA bundle
for the whole graph. The run record includes the effective CA pin.
Review, build, and run require the manifest to retain a locked CA pin.
To rotate the pin, update the manifest from the trusted release channel
and run `grimoire lock`. Locking verifies the new CA bytes and applies the
existing snapshot floor. Snapshot recovery requires the current locked CA
pin. For a lock written before this field was introduced, run
`grimoire lock` to record the manifest pin before a deployment audit.
Grimoire refuses cyclic requirements and requests for conflicting package
versions before publishing the lock. If the mirror updates during a
resolution, retry the command. Later resolutions
verify current metadata, so removing a target prevents further builds of that
version. The lock carries its verified snapshot version and digest across
machines. A build rejects an older version or different metadata at the same
version. Cached metadata preserves additional TUF rollback floors across runs
on the same machine. Switch trust pins only after independently reviewing
the replacement trust files and explicitly replacing the project lock.

Grimoire gives each TUF dependency resolution a default deadline of 60
seconds. The direct and transitive packages share that elapsed-time budget.
Set `resolution-deadline-ms` under `[registry]` to change it. The accepted
range is 1 to 2147483647 milliseconds. On expiry, Grimoire cancels the active
fetch and refuses the graph without replacing the project lock. Verified
metadata already committed to the local cache remains available for future
resolves. Individual Coven worker limits also apply.

During `grimoire web build`, the renderer's setting also caps each of
the two complete TUF catalog refreshes. Each refresh uses the shorter
of that setting and 60 seconds. If a refresh exceeds the limit,
Grimoire closes the active transport and leaves the published web
generation pointer alone.

The web build compiles each executable with the same manifest,
grant document, and lock bytes it checked before the first refresh.
It compares those files again just before making the staged
generation public. Save project edits and run the build again if
it reports that a checked input changed.

Use `grimoire run --record run.json` to retain the authenticated snapshot that
supplied the build, the input lock digest, and the digest of each package's
source. The build can verify unchanged pinned releases against a newer snapshot
without rewriting the lock.

### Recover a project lock after metadata signing-key retirement

Obtain the SHA-384 digest of the replacement snapshot through the registry
operator's trusted incident channel. The digest covers the complete
snapshot file. From the project with its existing lock, run:

```sh
grimoire lock --recover-tuf-snapshot "sha384:$replacement_snapshot_digest"
```

Coven still verifies the live root chain and current metadata under the
configured trust anchors. Grimoire requires the replacement digest on
every fetch and checks every existing package coordinate, sealed target,
release digest, publisher and dependency edge. Recovery replaces the
snapshot pin and version only after the entire locked graph matches.
It preserves the registry and toolchain pins. A failed recovery leaves the
lock intact. Review the resulting lock before sharing it.

### Verify an installed client against the HTTPS mirror

From the monorepo root, use the Python fixture environment described in
[Approval](approval.md). Set `WITCHYC` and `WITCHY_WASMTIME_HOST`. Select
the proof checker with `WITCHY_WASM_LEAN_ROOT` or `WITCHY_WASM_READY_BINDING`.

```sh
python3 coven/witchy2/tests/tuf_https_witness.py
```

The witness uses `install.py --from-bundle` with a detached installer, a
temporary tool prefix, home, proof cache, and registry state. Pass
`--client-lean-root /absolute/trusted-sdk` to check the installed
client with a separately provisioned full Lean SDK. Pass `--tuf-workers`
with an existing checked worker directory to exercise the bundled client.
This includes killing an installed `grimoire lock` during an authenticated,
incomplete download and checking that the connection closes without
publishing a lock, release, or root revision.
Use `--client-python /absolute/bin` to run the installer and installed
tools with that directory's `python3`. A separate Python environment
with only the standard library checks that the client can run apart
from the fixture's signing and certificate dependencies.

The witness uses a local CA and a live Witchy TLS listener. It publishes two
witnessed packages with a transitive dependency and checks the installed
client's output, exact source and release digests, and authenticated build
snapshots before and after a mirror update with the same lock. It also checks
refusal of rollback on a client with an empty cache, changed metadata and
target bytes, a root file that differs
from its independent pin, and an unrelated TLS CA. It also tries a timestamp
signed by the Targets key and a valid mirrored release whose publisher is
absent from the client's independently pinned release trust.

With `--web` or `--chromium CHROME`, the witness also publishes the next
snapshot while the HTTPS service is running. It verifies that the mirror
withholds the old catalog, continues serving TUF metadata and package
bytes, and serves the catalog again after installing the matching build.
