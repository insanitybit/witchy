# Deploy the public Coven mirror on Linux

On a Linux machine with systemd, install the Coven mirror runtime under
`/opt/coven`. On the public origin a socket forwards TCP to the
loopback mirror inside a private network namespace. Witchy handles TLS,
HTTP, and the public files. The forwarding service has no TLS key and
does not receive plaintext. The service units use the Debian systemd
binary path. [systemd socket proxy](https://www.freedesktop.org/software/systemd/man/latest/systemd-socket-proxyd.html)

## Build and install the runtime

On the trusted build machine use the matching Witchy compiler and Linux
Wasmtime host for the destination architecture. The build command checks
the mirror policy, compiles the Witchy worker, and packages the host,
launcher, module pin, and systemd units.

```sh
WITCHYC=/path/to/witchyc deploy/witchy2/build-runtime.sh mirror \
  /path/to/linux/witchy-wasmtime-host ./mirror-release
```

Transfer `mirror-release/coven-mirror.tar` and provision its SHA-384 digest
through the trusted release channel. The build records the digest in
`SHA384SUMS`. On the destination set `archive_pin` to the independently
provisioned `sha384:<digest>` value. Verify the archive before extracting it
into a private empty directory. Use a new `/opt/coven` on first install.

```sh
set -e
archive=./coven-mirror.tar
printf '%s  %s\n' "${archive_pin#sha384:}" "$archive" | sha384sum --check --strict
bundle=$(mktemp -d)
tar --same-permissions --no-same-owner -xf "$archive" -C "$bundle"
sudo test ! -e /opt/coven
sudo cp -a --no-preserve=ownership "$bundle/opt/coven" /opt/
sudo install -d -m 0755 /etc/coven
sudo install -m 0600 "$bundle/pins/mirror.env" /etc/coven/mirror.env
sudo install -m 0644 "$bundle/systemd/"* /etc/systemd/system/
```

On upgrades, stop the affected service and socket, install a fresh runtime
tree and its pins together, then reload systemd and start the service and
socket. The archive carries no TLS identity, registry trust or stored data.

## Prepare the public tree

- Publish the TUF repository under `/srv/coven/published`. To serve its
  catalog, run `coven web install ./site/dist --mirror /srv/coven/published`.
  Use the separate [publisher archive](#publish-from-an-operator-machine)
  to publish from an operator machine without the compiler.
  Publication and site installs run as the publication operator. The
  mirror account needs only read and traverse access to the public tree.
- Create a system user and group named `coven-mirror`. Provision the
  P-384 PKCS#8 DER key at `/etc/coven/keys/mirror.der`, owned by that user
  with mode `0400`. Keep CI, maintainer, publisher and TUF signing keys
  outside the service and the public tree.
- Set `tls/authority` in the public tree to the external DNS name with
  `:443`. The file accepts the complete DNS name and port with an
  optional LF or CRLF line ending. Install the verified public chain as
  `tls/chain` using the preparation step below.

## Publish from an operator machine

Build the signerless publisher on the trusted build machine. Provide the
Linux Wasmtime host for the operator machine's architecture.

```sh
WITCHYC=/path/to/witchyc deploy/witchy2/build-runtime.sh publisher \
  /path/to/linux/witchy-wasmtime-host ./publisher-release
```

Transfer `coven-publisher.tar` and provision its SHA-384 digest through
the trusted release channel. Verify the archive as described under
[Build and install the runtime](#build-and-install-the-runtime), then
extract it into a private operator directory. It ships the launcher,
Wasmtime host, and policy-checked publication worker. The archive carries
no signing keys or service units. The launcher checks the independently
pinned module at staging and Wasmtime verifies the same pin at admission.
The publisher commits the timestamp only after the worker has verified
and staged the complete update. The launcher needs Python 3.11 or later
and uses its standard library. Run it as the account that owns the published
tree, with `publisher_bundle` set to the extracted directory.

```sh
. "$publisher_bundle/pins/publisher.env"
runtime="$publisher_bundle/opt/coven"
WITCHY_WASMTIME_HOST="$runtime/bin/witchy-wasmtime-host" \
  "$runtime/bin/coven" tuf publish --incoming "$incoming" --mirror "$published" \
  --root "$root_file" --root-pin "$root_pin" --trust "$trust_file" --trust-pin "$trust_pin" \
  --module "$runtime/libexec/coven-publisher/tuf-prepare-repository.wasm" \
  --module-pin "$COVEN_TUF_PUBLICATION_PIN"
```

The signed incoming directory and the independently provisioned trust
files are described in [Publish a signed update](../../witchy2/tuf.md#publish-a-signed-update).
Keep the publisher installation and its incoming directory outside the
read-only mirror's served tree.

## Provision the release workers

Build the release archive on the trusted build machine.

```sh
WITCHYC=/path/to/witchyc deploy/witchy2/build-runtime.sh release \
  /path/to/linux/witchy-wasmtime-host ./release-workers
```

Transfer `coven-release.tar` and verify its independently provisioned
SHA-384 digest before extracting it into a private directory. The
archive carries the launcher, Wasmtime host, and the policy-checked
workers for package signing, approval sealing, delegated authorization,
and policy rotation. It carries no signing keys or service units.
Provision each key only on the machine that uses it for that role.

```sh
. "$release_bundle/pins/release.env"
runtime="$release_bundle/opt/coven"
workers="$runtime/libexec/coven-release"
export WITCHY_WASMTIME_HOST="$runtime/bin/witchy-wasmtime-host"
"$runtime/bin/coven" tuf sign-package candidate.coven --registry-id "$registry_id" \
  --module "$workers/publisher-sign.wasm" --module-pin "$COVEN_TUF_PUBLISHER_SIGN_PIN" \
  --key publisher.seed --out publisher.sig
```

For [sealing](../../witchy2/tuf.md#sign-and-seal-without-a-compiler), use
`witness-seal.wasm` with `COVEN_TUF_WITNESS_SEAL_PIN` and
`seal-snapshot.wasm` with `COVEN_TUF_SEAL_SNAPSHOT_PIN`.
For [delegation and rotation](../../witchy2/tuf.md#authorize-rotate-and-publish-without-a-compiler),
use `delegate-prepare.wasm`, `delegate-sign.wasm`, and
`delegate-finalize.wasm` with the corresponding `COVEN_TUF_DELEGATE_*`
pins, or `rotate-policy.wasm` with `COVEN_TUF_ROTATE_POLICY_PIN`.
Use the publisher archive on the machine that installs the signed files
into the public tree.

## Refresh the public catalog

First publish the signed TUF metadata. Build the Glamour catalog against
that publication with `grimoire web build` and install it with
`coven web install`. The installer holds the publication lock while it
matches the timestamp and snapshot, installs the asset bodies, and
replaces the route table. The new route table pins the entire published
timestamp file by its SHA-384 digest.
New framed catalogs also pin the exact route rows with a SHA-384 digest.
The mirror checks that routes appear once in installer order and that
the table includes all fixed routes for its profile. A malformed
or incomplete table returns a bodyless `503` with `Cache-Control: no-store`
on catalog routes. TUF metadata and package downloads keep their own
route handling.

The installer and HTTPS mirror both check the shipped Glamour bootstrap,
worker, and frame against their own pins. A generation receipt or route
table cannot authorize different executable assets. Upgrade the Grimoire
and Coven runtimes together when these shipped assets change, then rebuild
and install the catalog with the matching tools.

After a publication or timestamp renewal, the HTTPS mirror returns
`503` with `Cache-Control: no-store` for stale catalog routes until a
matching catalog is installed. TUF metadata and package routes stay
available. A browser request that already checked the old timestamp
can finish with the matching old asset. If a loaded catalog loses
access to a filter asset, it keeps all rows visible and reports that
filtering is unavailable.

Update the mirror runtime before installing a newly formatted table.
The current mirror accepts both earlier route-table profiles. Reinstall
the checked catalog with the new launcher to add the row digest. A catalog
built against the current timestamp can be reused. Rebuild it if the
publication has changed.

## Prepare or renew the TLS identity

Request a certificate for the public DNS name with an ECDSA P-384 leaf
key. For Certbot, set `--key-type ecdsa --elliptic-curve secp384r1` when
requesting the certificate. Certbot uses P-256 by default for new keys.
[Certbot certificate key options](https://eff-certbot.readthedocs.io/en/stable/using.html#rsa-and-ecdsa-keys)

Set `key_der` to the service's private PKCS#8 DER key. If your issuer's
client writes PEM, set `issued_key` to that PEM key and convert it as the
key owner. [OpenSSL PKCS#8 options](https://docs.openssl.org/3.5/man1/openssl-pkcs8/)

```sh
umask 077
candidate=$(mktemp -d)
key_der="$candidate/server.der"
openssl pkcs8 -topk8 -nocrypt -in "$issued_key" -outform DER -out "$key_der"
chmod 0400 "$key_der"
```

Run `coven tls-prepare` as the owner of the new P-384 PKCS#8 DER key.
Give it a leaf-first PEM chain and the independently selected client trust
anchors. The Witchy worker verifies the certificate path, DNS name, validity,
and possession of the matching signing key. It creates a new public
length-prefixed DER file. The secret key is opened by descriptor. The
worker returns the verified public bytes over a bounded output pipe, and
the launcher creates the candidate file after the worker succeeds. The worker
stops if its launcher exits or its own deadline expires.

Use a copy of the intended client trust bundle for `trusted-issuers.pem`.
The PEM loader admits up to 512 KiB and 256 certificates. Install the
independently selected bundle as the mirror's startup trust anchors.
The archive contains the verification worker and its pin, while the
deployment machine supplies the trust anchors.

Provision `mirror_ca_pin`, the SHA-384 digest of the approved issuer
PEM bytes, independently with the trust material. Append it after
installing `pins/mirror.env` from the archive, including during upgrades.

```sh
ca_hex=${mirror_ca_pin#sha384:}
case "$ca_hex" in *[!0-9a-f]*) exit 1 ;; esac
test "$mirror_ca_pin" = "sha384:$ca_hex" && test "${#ca_hex}" -eq 96 || exit 1
sudo install -m 0644 trusted-issuers.pem /etc/coven/mirror-ca.pem
printf 'COVEN_MIRROR_TLS_CA_PIN=%s\n' "$mirror_ca_pin" | sudo tee -a /etc/coven/mirror.env > /dev/null
```

Review the candidate output before changing the live identity.

On the deployment machine, load `pins/tls.env` from the verified runtime
archive. Wasmtime checks the pin against the bytes it loads before the
worker can use the signer. Use the real HTTPS service origin, including
any nondefault port.

```sh
. "$bundle/pins/tls.env"
WITCHY_WASMTIME_HOST=/opt/coven/bin/witchy-wasmtime-host \
  /opt/coven/bin/coven tls-prepare --origin "https://$public_host" \
  --fullchain fullchain.pem --ca-file trusted-issuers.pem --key "$key_der" \
  --module /opt/coven/libexec/coven-tls-prepare.wasm --module-pin "$COVEN_TLS_PREPARE_PIN" \
  --out verified-chain.derlist
```

During renewal, stop the affected service and its ingress socket before
changing the key and chain. Install the chain at
`/srv/coven/published/tls/chain` and the key at
`/etc/coven/keys/mirror.der` with its service ownership and private mode.
When renewing the issuer bundle, update its file and independently
approved pin together.
Start the service and socket, then verify from a client over HTTPS.
Remove the temporary DER copy after installing it if you converted a PEM key.

## Start and verify

If `systemd-socket-proxyd` lives at a different path on your host, update
`coven-public.service`.

```sh
systemd-analyze verify /etc/systemd/system/coven-mirror.service \
  /etc/systemd/system/coven-public.service /etc/systemd/system/coven-public.socket
systemctl daemon-reload
systemctl enable --now coven-mirror.service coven-public.socket
```

Before reporting readiness, the launcher runs the pinned Witchy TLS
check against the installed authority, chain and signer. The worker
verifies the chain against `/etc/coven/mirror-ca.pem` and checks the
hostname, current validity, and possession of the matching key. It
reports the SHA-384 of the exact PEM bytes it used. The launcher compares
that digest with `COVEN_MIRROR_TLS_CA_PIN` before it starts the listener.
It waits up to ten seconds for the host's complete readiness token and
relays readiness to systemd only while the host is still running.

From a client that can reach the public ingress, use the verified mirror
archive for the client's architecture to check the timestamp and catalog
routes. Use the public service origin, including any nondefault port, and
independently provisioned client CA roots and their SHA-384 pin. The CA
file is the client's trust input.

```sh
. "$bundle/pins/probe.env"
runtime="$bundle/opt/coven"
export WITCHY_WASMTIME_HOST="$runtime/bin/witchy-wasmtime-host"
for route in timestamp catalog; do
  "$runtime/bin/coven" mirror-check --origin "https://$public_host" --route "$route" \
    --ca-file "$ca_file" --ca-pin "$ca_pin" \
    --module "$runtime/libexec/coven-mirror-check.wasm" --module-pin "$COVEN_MIRROR_CHECK_PIN"
done
```

The probe checks the TLS identity, HTTP 200, Content-Length,
Content-Type, and Cache-Control. It prints the status and representation
length as JSON without downloading the body. Check the catalog route
after installing the catalog. The probe needs no compiler, signing key,
or registry state.

Use the verified client bundle from the next section to authenticate the
publication. Obtain `snapshot_pin` and `timestamp_pin` from the
signed publication through the trusted release channel. Supply the
independent TUF root and release-trust files and pins for that registry.

```sh
WITCHY_WASMTIME_HOST="$prefix/bin/witchy-wasmtime-host" \
  "$prefix/bin/coven" tuf catalog --registry "https://$public_host" --id "$registry_id" \
  --root "$root_file" --root-pin "$root_pin" --trust "$trust_file" --trust-pin "$trust_pin" \
  --expect-snapshot "$snapshot_pin" --expect-timestamp "$timestamp_pin" >/dev/null
```

For a private CA, add `--ca-file "$ca_file" --ca-pin "$ca_pin"`.
Use `--ca-pin` alone to pin the bytes read from the default PEM path.
The HTTPS catalog receipt includes the effective PEM digest as `ca_pin`.
The Glamour build pins that bundle for the renderer, browser filter, and
final catalog recheck. The command verifies the signed metadata and its expiry before checking
the exact digests. A timestamp renewal requires the new timestamp pin
even if the snapshot is unchanged. The fresh-consumer workflow below
also verifies the package bodies it builds from.

The forwarder limits concurrent forwarding. The mirror applies its
worker, fuel, byte and deadline limits. TCP forwarding preserves the
client's SNI and TLS stream. The Witchy listener sees the local forwarder
as the peer address.

The mirror uses read-only grants for the public tree and an opaque TLS signer.
The launcher keeps a lifeline open for the Wasm host and closes it on shutdown.
The deployment does not run a compiler, contact the CI service, or open the
maintainer database.

After the first HTTPS request has activated the proxy, check the live
processes as root. The mirror and proxy should share a network namespace
separate from PID 1. Both should have no effective capabilities and have
`NoNewPrivs` enabled.

```sh
set -e
mirror_pid=$(systemctl show coven-mirror.service -p MainPID --value)
proxy_pid=$(systemctl show coven-public.service -p MainPID --value)
test "$mirror_pid" -gt 0
test "$proxy_pid" -gt 0
network=$(readlink "/proc/$mirror_pid/ns/net")
test "$network" = "$(readlink "/proc/$proxy_pid/ns/net")"
test "$network" != "$(readlink /proc/1/ns/net)"
for pid in "$mirror_pid" "$proxy_pid"; do
    grep -Eq '^NoNewPrivs:[[:blank:]]+1$' "/proc/$pid/status"
    grep -Eq '^CapEff:[[:blank:]]+0+$' "/proc/$pid/status"
done
```

### Verify from a fresh consumer

From a separate consumer machine, [install the pinned client bundle](../../../grimoire/docs/coven-http.md#install-from-a-pinned-client-bundle)
with the four TUF workers and select the trusted proof checker. Use the
public HTTPS origin in the project's registry settings. Provision its
TUF root and release-trust files and their pins through the trusted
release channel. Provision the SHA-384 pin of the approved PEM bundle
through the same channel and set `ca-pin` in the consumer manifest. Set
`ca-file` when provisioning a separate PEM file. With `ca-pin` alone,
Grimoire requires the default bundle selected on the consumer to match
the independent pin, including when `SSL_CERT_FILE` selects the file.
An approved platform bundle update requires reissuing the manifest and
CA pin through the trusted release channel. Run `grimoire lock` with the
updated manifest to verify the approved bundle and record its new pin
while preserving the existing snapshot floor.

Use a sample executable that imports a signed library with a transitive
dependency and has known output. [Consumer manifest and trust settings](../../witchy2/tuf.md#consume-releases-through-grimoire)

Provision an `expected.json` release inventory independently of the
mirror and the run under test. Prepare it from the reviewed project
sources, signed release inputs, and signed snapshots. Prepare the
expected run snapshot version through the same trusted channel. The
inventory contains these fields.

| Field | Value |
| --- | --- |
| `registry` | The exact HTTPS `url`, registry `id`, `root-pin`, `trust-pin`, and independently provisioned `ca-pin`. The CA pin covers either the configured PEM file or the approved platform PEM bundle. |
| `snapshots` | A map from decimal version strings to SHA-384 digests of the exact signed snapshot documents. Include both the locked snapshot and the expected run snapshot if they differ. |
| `packages` | A map keyed by package name, including transitive dependencies. Each value has `version`, `requires` (an array of locked requirements such as `helper 1.0.0`), and the `digest`, `target`, `publisher`, and `source` pins for the signed package, sealed target, publisher public key bytes, and entry source. For a multi-file package, include a `files` map from each source path to its digest. |
| `sources` | A map from each project source path, relative to the entry file's directory, to its digest. |
| `manifest` | The SHA-384 digest of the exact reviewed project manifest, including its chosen registry trust and grant settings. |
| `grants` | The SHA-384 digest of the exact reviewed grant document. If the manifest specifies no grant document, use the digest of zero bytes. |
| `stdout` | Exact expected UTF-8 output, including the final newline if the program writes one. |

Supply a separate `toolchain.json` for the selected client platform.
It contains `compiler` and `host` pins for the exact `bin/witchyc` and
`bin/witchy-wasmtime-host` bytes in the independently pinned client
bundle. Include a `host-libraries` object mapping each bundled crypto
library filename to its pin. Use an empty object for the Linux bundle
when it has no separate crypto library. Compute these digests from the
trusted bundle contents before distributing the pins.

Write each digest as `sha384:` followed by 96 lowercase hexadecimal
characters. Copy `coven/witchy2/tests/check_deployed_tuf_run.py` from
the reviewed checkout to the consumer. It uses the Python standard
library. The audit compares the exact bytes of the local manifest,
provisioned trust files, listed project sources, grant document, and
capability policy with their independent pins. It requires regular files
and bounds its reads. It checks the entry name and compares the manifest
and TUF registry identity with the run record and lock. The audit
compares the independent CA pin with the manifest, the lock, and the
effective PEM digest in the run record. When the manifest supplies a PEM path, the
audit also checks the file directly.

On the consumer, lock the toolchain and inspect the source and
capability policy for the full graph. First run the reviewed installer's
read-only bundle verification with the independent bundle pin. Use that
prefix for Grimoire, and select its Coven, compiler, and Wasmtime host
for the commands. Use `grimoire review --write-policy`
to draft a missing policy, then review it before building. Set
`policy_pin` to the SHA-384 pin of the exact approved policy bytes.
Set `expected_snapshot` to the version from the trusted release channel.
Set `expected_snapshot_pin` to its signed snapshot digest from the
independently provisioned `expected.json` inventory. Set `manifest_pin`
and `grants_pin` to that inventory's `manifest` and `grants` fields.

```sh
python /path/to/install.py --verify-bundle /path/to/witchy-client.zip \
  --bundle-pin "$bundle_pin" --prefix "$prefix" &&
export PATH="$prefix/bin:$PATH" COVEN="$prefix/bin/coven" \
  WITCHYC="$prefix/bin/witchyc" WITCHY_WASMTIME_HOST="$prefix/bin/witchy-wasmtime-host" &&
grimoire lock --toolchain &&
python /path/to/check_deployed_tuf_run.py /path/to/expected.json . "$expected_snapshot" \
  --toolchain /path/to/toolchain.json --policy-pin "$policy_pin" --preflight &&
grimoire review &&
grimoire build -o target/deployment.wasm &&
module_pin=$(python -c 'import hashlib, sys; print("sha384:" + hashlib.file_digest(sys.stdin.buffer, "sha384").hexdigest())' \
  < target/deployment.wasm) &&
grimoire run --tuf-snapshot "$expected_snapshot" "$expected_snapshot_pin" \
  --launch-pins "$manifest_pin" "$grants_pin" \
  --module-pin "$module_pin" --record run.json > run.stdout &&
python /path/to/check_deployed_tuf_run.py /path/to/expected.json . "$expected_snapshot" \
  --toolchain /path/to/toolchain.json --policy-pin "$policy_pin" \
  --built-module target/deployment.wasm
```

Preflight compares the local inputs and lock with the independently
approved inventory before building or running the program. With the
default issuer bundle, run the check in the same environment and working
directory as Grimoire so it checks the same PEM file. The module pin
makes Grimoire refuse a rebuild that differs from the retained Wasm
before Wasmtime starts the program. The TUF snapshot pin requires the
mirror to serve the approved signed snapshot for the entire dependency
graph during the run, even when the existing lock allows a newer snapshot.
Grimoire also checks the launch pins against the exact manifest and grant
document bytes it captures, before reading secrets or building. The
post-run audit binds the output and retained Wasm to the run record.

Retain both inventories, the reviewed policy and its pin, `witchy.lock`,
`target/deployment.wasm`, `run.json`, and `run.stdout`. The checker verifies
that the build record corresponds to the lock and matches the trusted
compiler, host, registry,
snapshot, package graph, source digests, manifest, grant document,
capability policy, and output. It verifies the host-library pins in the
lock, which Grimoire checks before running. It also requires the recorded
module digest to match the SHA-384 pin checked by the host. The
`--built-module` check confirms that the retained build and the module
admitted during the separate recorded run have identical bytes. Grimoire
captures the build inputs before starting the module and publishes the
record only after successful execution. The checker requires the completion
field.
An unsuccessful run leaves any prior record intact. Run the checker
only after the current `grimoire run` exits successfully.
A newer snapshot can satisfy the existing lock when both snapshots appear
in the inventory and the pinned releases remain unchanged. Grimoire
authenticates metadata and packages during resolution. Run the audit
on the trusted consumer that produced the record and output.

### Reproduce the consumer check on a disposable systemd VM

On the build machine, set `WITCHYC`, `WITCHY_WASMTIME_HOST`, and the
trusted proof checker environment. Use the fixture Python environment
from the [approval tests](../../witchy2/approval.md).

```sh
scratch=$(mktemp -d)
python coven/witchy2/tests/tuf_https_witness.py \
  --export-systemd-fixture "$scratch/fixture" --fixture-port 18443
```

The export uses fresh synthetic signing keys and a fixture CA. It writes
`published/` for snapshot 1, `published-new/` for snapshot 2, and
`mirror.der` for the guest's test TLS key. Point the disposable VM's
mirror at the first publication and its test key using the installation
steps above. The leaf names `localhost` and uses a P-384 key. Its RSA intermediate
is served with the leaf. The separately provisioned ECDSA root is in
the fixture's `project/ca.pem`. Install that PEM bundle as
`/etc/coven/mirror-ca.pem` on the VM and provision it separately to the
consumer. The guest's unmodified public socket listens on port 443.
For a separate consumer, keep the fixture's external port at 18443 and
forward it to the guest's public socket over SSH. Run this on the
consumer with `test_guest` set to the disposable VM's SSH host.

```sh
ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:18443:127.0.0.1:443 "$test_guest"
```

The consumer uses the fixture origin `https://localhost:18443` and
still checks the TLS name and chain with its separately supplied CA.
The guest ingress forwards the encrypted bytes into the private
mirror namespace. For a consumer on the guest itself, use a separate
account and export with `--fixture-port 443` for the canonical
`https://localhost` origin. Add `--platform-ca-file /path/to/ca-bundle.pem`
to exercise the platform PEM input alongside the fixture root. Keep
`mirror.der` on the test server side and remove it after the run.

Add `--fixture-default-ca` to test the pinned default CA path. The
export omits `ca-file` from the project manifest and records the
manifest and PEM digests in `expected.json`. On the consumer, point
`SSL_CERT_FILE` at the independently provisioned fixture `ca.pem` using
an absolute path before locking, reviewing, building, or running.
Use the same bundle for all four operations.

Send a copy of `project/` and `expected.json` separately to the consumer.
Prepare `toolchain.json` from the trusted bundle for the consumer's platform.
In a fresh, unprivileged consumer account, install the independently
pinned client bundle for its platform and provision the
[trusted proof checker](../../../grimoire/docs/coven-http.md#install-from-a-pinned-client-bundle).
For Linux ARM64, the full Lean SDK can be provisioned from a verified
archive before the test. The Linux fixture uses a VM with two vCPUs and
4 GiB of RAM and keeps the extracted SDK on the guest's local disk. Set
`WITCHY_WASM_LEAN_ROOT` to the absolute SDK directory and run the
installer's proof self-check. The standalone checker fingerprints the
full SDK before and after proof checking. Both reads count toward the
build time. [Proof phase measurements](../../../witchyc/docs/wasm-phase-measurements.md)

Put the installed `bin/` on `PATH`. In the copied project, run:

```sh
grimoire lock --toolchain &&
grimoire review --write-policy &&
grimoire review
```

Review the generated policy before recording `policy_pin`. Set
`expected_snapshot_pin` to the digest for version 1 from `expected.json`,
and set `manifest_pin` and `grants_pin` from that inventory. Then
preflight, build, run, and audit the fixture.

```sh
python /path/to/check_deployed_tuf_run.py /path/to/expected.json . 1 \
  --toolchain /path/to/toolchain.json --policy-pin "$policy_pin" --preflight &&
grimoire build -o target/deployment.wasm &&
module_pin=$(python -c 'import hashlib, sys; print("sha384:" + hashlib.file_digest(sys.stdin.buffer, "sha384").hexdigest())' \
  < target/deployment.wasm) &&
grimoire run --tuf-snapshot 1 "$expected_snapshot_pin" \
  --launch-pins "$manifest_pin" "$grants_pin" \
  --module-pin "$module_pin" --record run.json > run.stdout &&
python /path/to/check_deployed_tuf_run.py /path/to/expected.json . 1 \
  --toolchain /path/to/toolchain.json --policy-pin "$policy_pin" \
  --built-module target/deployment.wasm
```

Save copies of `witchy.lock` and the successful `run.json`. On the
disposable consumer, change the manifest and grant document one at a
time, restoring each after its test. Repeat the pinned run with the
original approved pins. Both changes must be refused with no guest
output and the same successful record. Repeat with a wrong module pin
and verify the same result.

Publish the fixture's second snapshot and timestamp. Repeat the pinned
run with the original snapshot pair. The mirror now serves snapshot 2,
so Grimoire must refuse before guest execution. Use fresh Grimoire state
for this check so the observed refusal is for the supplied pin. Repeat with version 1 and the digest of snapshot 2 to
check that both parts of the expected snapshot are enforced. Both
refusals must preserve the successful record.

Run again with the existing lock, passing version 2 and its approved
digest to `--tuf-snapshot` and `2` to the checker. Verify that the lock
is byte-for-byte unchanged. Run `grimoire lock` to advance to snapshot 2,
then repeat the recorded run and the checker.
After saving the updated lock, restore the first signed timestamp. Run a
build in fresh state and cache directories with the snapshot 2 project lock
still in place. Expect a rollback refusal and verify that the lock is
byte-for-byte unchanged and the requested output is absent.

```sh
state=$(mktemp -d)
cache=$(mktemp -d)
XDG_STATE_HOME="$state" XDG_CACHE_HOME="$cache" grimoire build -o refused-rollback.wasm
```

Restore snapshot 2 and change a sealed package in the disposable mirror's
public tree without changing its name or length. Repeat the build with
fresh directories and a fresh output path. Expect a release evidence
refusal. Restore the sealed package before using the mirror again.

### Build the fixture catalog on the detached client

Add `--web` to the fixture export command to publish a signed Glamour
release alongside the sample project dependencies. After advancing the
mirror to snapshot 2, follow the [Glamour catalog build](../../witchy2/web/README.md)
with the detached tools. Use the fixture's HTTPS origin, registry ID,
root, release trust, and CA pins. Lock and review both the catalog and
its browser project before building. Hand the checked `dist/` directory
to the publication operator to install against snapshot 2.

Use the same separately provisioned CA to fetch the catalog, sandboxed
frame, and filter Wasm. Compare their bodies with `index.html`,
`glamour-frame.html`, and `filter.wasm` from the checked generation.
The catalog CSP requires Trusted Types for script and the frame CSP
includes `sandbox allow-scripts`.

### Check cancellation with a separate test identity

Export a second synthetic fixture for the cancellation test. Its key
belongs to the local listener started by the witness. The deployed mirror
keeps its own test key. Run the check as the test consumer using the
pinned installation. It closes HTTPS when the launcher exits or the
resolution deadline expires. The check also verifies that the incomplete
response never becomes a root revision, release, or lock.

```sh
python coven/witchy2/tests/tuf_https_witness.py \
  --export-systemd-fixture "$scratch/cancellation-fixture"
python coven/witchy2/tests/check_tuf_cancellation.py "$scratch/cancellation-fixture" /path/to/installed/bin/grimoire
```

Remove both synthetic test keys when the fixture services have stopped.

For the separate CI and maintainer origins, see the [private deployment units](PRIVATE.md).
