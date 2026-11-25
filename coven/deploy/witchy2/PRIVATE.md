# Deploy Coven's private origins on Linux

The CI staging and maintainer review services use separate DNS names,
certificates, TLS keys and private network namespaces. The supplied TCP
ingress sockets bind host loopback. Give authorized runners and maintainers
access through your private network. Witchy handles TLS and checks the
configured authority on each origin. The forwarders operate on encrypted
TCP streams. [systemd namespace sharing](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#JoinsNamespaceOf=)

## Provision accounts and inputs

Build the private runtime archive on the trusted build machine.

```sh
WITCHYC=/path/to/witchyc deploy/witchy2/build-runtime.sh private \
  /path/to/linux/witchy-wasmtime-host ./private-release
```

Verify `coven-private.tar` using the same independent archive-pin procedure
as the [mirror deployment](README.md#build-and-install-the-runtime).
For a dedicated private host, install the extracted `opt/coven` and
`systemd` trees as shown there. If the host already runs the public mirror
from the same build, compare the shared files with the verified private
bundle and install the private files separately. Run the following
checks in a shell with `set -e` before copying anything:

```sh
shared="$bundle/opt/coven"
cmp "$shared/bin/witchy-wasmtime-host" /opt/coven/bin/witchy-wasmtime-host
cmp "$shared/libexec/coven-tls-prepare.wasm" /opt/coven/libexec/coven-tls-prepare.wasm
for file in "$shared"/libexec/coven/coven "$shared"/libexec/coven/*.py; do
    cmp "$file" "/opt/coven/libexec/coven/${file##*/}"
done
sudo cp -a --no-preserve=ownership "$shared/libexec/coven-private" /opt/coven/libexec/
sudo cp -a --no-preserve=ownership "$shared/libexec/coven/release_web" /opt/coven/libexec/coven/
sudo install -m 0644 "$bundle/systemd/"* /etc/systemd/system/
```

If a comparison differs, schedule a coordinated replacement of the
shared runtime. Stop the public and private services and their ingress
sockets before replacing the runtime or its module pins. The CI and review
launchers use the matching host's readiness signal to tell systemd when
their private listeners have bound. The host opens each listener module as
a bounded regular file and verifies its independent SHA-384 pin against
the bytes it compiles. Install the module and its pin together.
The CI identity and storage launchers and the maintainer review
launchers also pass their pins to the host on every invocation. This
covers challenge, inspection, approval, and TUF baseline workers.
The host refuses changed module bytes before guest execution.

The private archive includes the listener, CI identity, and review workers
as well as the schema, policy provisioning, backup manifest, and recovery workers. The builder checks
every worker against its checked-in capability policy before building it.

Create a `coven-staging` system user and group and an owner-only
`/var/lib/coven-private/staging` directory. Use this account when running
local staging and maintainer administration commands. The two brokers
use this same account because they share the owner-only SQLite store.
They and the staging account belong to the same trust boundary. The two
Witchy guests receive separate host grants. The units give the review
broker read access to `/srv/coven/published`, where it verifies its
comparison baseline.

Provision the site-specific inputs below separately from the runtime.

| Path | Inputs |
| --- | --- |
| `/etc/coven/ci/` | `config.json`, `github-jwks.json`, `nonce.key`, `chain.derlist`, `tls.der`, `pins.env` |
| `/etc/coven/review/` | `release-trust.json`, `initial-root.json`, `chain.derlist`, `tls.der`, `pins.env` |

Install the CI configuration and JWKS as separate regular files owned by
`coven-staging`. Mode `0400` gives the service read access. Keep their
independent pins in the root-owned environment file.

Both TLS keys are owner-only P-384 PKCS#8 DER files owned by
`coven-staging`. The CI nonce is the independent owner-only 32-byte
key from the CI instructions. Use the [same pinned TLS preparation worker](README.md#prepare-or-renew-the-tls-identity)
for each origin, passing its own key, full chain and exact origin. Install
the resulting chains as `chain.derlist`. Stop the affected private service
and socket before replacing the pair.
Keep publisher, witness, and TUF signing keys outside both services.

Install the verified archive's `pins/ci.env` and `pins/review.env` as the
root-owned `pins.env` files for the two services. Add the independently
provisioned `COVEN_CI_CONFIG_PIN` to the CI pin file, and
`COVEN_RELEASE_TRUST_PIN` and `COVEN_RELEASE_ROOT_PIN` to the review pin
file. Use SHA-384 pins in the `VAR=sha384:<digest>` form.

Load `pins/provision.env` from the verified archive into the operator shell.
Initialize or upgrade the staging store and install the review policy as
`coven-staging` before starting the listeners. Set `review_policy` to the
approved policy JSON file, readable by the staging account.

```sh
. "$bundle/pins/provision.env"
modules=/opt/coven/libexec/coven-private
sudo -u coven-staging env WITCHY_WASMTIME_HOST=/opt/coven/bin/witchy-wasmtime-host \
  /opt/coven/bin/coven release init --staging-dir /var/lib/coven-private/staging \
  --module "$modules/staging-schema.wasm" --module-pin "$COVEN_STAGING_SCHEMA_PIN"
sudo -u coven-staging env WITCHY_WASMTIME_HOST=/opt/coven/bin/witchy-wasmtime-host \
  /opt/coven/bin/coven release set-policy --staging-dir /var/lib/coven-private/staging \
  --module "$modules/review-set-policy.wasm" --module-pin "$COVEN_REVIEW_POLICY_MODULE_PIN" "$review_policy"
```

Use the [pinned operator commands](../../witchy2/approval.md#operate-without-a-compiler)
to prepare the local staging, enrollment, approval and receipt workers
used for administration. The local challenge issuer has a different scope
from the browser challenge worker in the runtime archive.
Distribute [pinned publisher and witness workers](../../witchy2/tuf.md#sign-and-seal-without-a-compiler)
to the release signing machines.

## Start the private services

```sh
systemctl daemon-reload
systemctl enable --now coven-ci.service coven-review.service \
  coven-private-ingress@ci.socket coven-private-ingress@review.socket
```

The supplied ingress addresses are `127.0.0.1:9443` for CI and
`127.0.0.1:9444` for review. The actual origins are the HTTPS hostnames
and ports in the pinned CI configuration and review policy. To bind a
private interface, install a drop-in for the corresponding socket.
Assign `ListenStream=` with an empty value before adding the new
address, so systemd replaces the supplied listener. Restrict access to
the authorized networks. When both origins use port 443, give them
distinct ingress addresses. Each backend binds `127.0.0.1:8443` in
its own namespace.

The ingress service waits for its backend's readiness notification. The
launcher sends readiness after the Witchy host is accepting loopback
connections. [systemd startup notification](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#Type=)

CI identity and storage workers and maintainer transaction and comparison
workers receive a pipe whose writing end belongs to the launcher. If
the launcher exits, Wasmtime sees EOF and stops the worker, including
during a blocked read. Workers also have their own deadlines. A
transaction already committed before shutdown retains its receipt.

Exercise the [CI staging and receipt flow](../../witchy2/ci-staging.md#stage-and-recover-a-receipt)
and the [maintainer review ceremony](../../witchy2/approval.md#maintainer-browser-review)
against their configured origins. Restart the affected backend after
replacing its TLS identity, pinned configuration, or worker modules.
The public catalog and TUF distribution use their own
[mirror deployment](README.md).

After HTTPS requests have activated both proxies, run the following as
root. Each listener should share a network namespace with its own proxy.
The two pairs should use separate namespaces from each other and from
PID 1. All four processes should have `NoNewPrivs` enabled and no
effective capabilities.

```sh
set -e
previous=
for role in ci review; do
    listener=$(systemctl show "coven-$role.service" -p MainPID --value)
    proxy=$(systemctl show "coven-private-ingress@$role.service" -p MainPID --value)
    test "$listener" -gt 0
    test "$proxy" -gt 0
    network=$(readlink "/proc/$listener/ns/net")
    test "$network" = "$(readlink "/proc/$proxy/ns/net")"
    test "$network" != "$(readlink /proc/1/ns/net)"
    test "$network" != "$previous"
    previous=$network
    for pid in "$listener" "$proxy"; do
        grep -Eq '^NoNewPrivs:[[:blank:]]+1$' "/proc/$pid/status"
        grep -Eq '^CapEff:[[:blank:]]+0+$' "/proc/$pid/status"
    done
done
```

## Back up and recover private staging

Run these commands as the `coven-staging` account. Load the recovery
worker pins from the independently verified private runtime archive.
Store the backup on private storage and retain the manifest pin returned
by `backup` through an independent channel.

```sh
. "$bundle/pins/recovery.env"
modules=/opt/coven/libexec/coven-private
coven release backup --staging-dir /var/lib/coven-private/staging \
  --out "$backup_directory" \
  --manifest-module "$modules/staging-backup-manifest.wasm" \
  --manifest-pin "$COVEN_STAGING_BACKUP_MANIFEST_PIN" \
  --database-module "$modules/staging-recovery.wasm" \
  --database-pin "$COVEN_STAGING_RECOVERY_PIN"
coven release restore --backup "$backup_directory" --backup-pin "$backup_pin" \
  --staging-dir "$recovered_directory" \
  --manifest-module "$modules/staging-backup-manifest.wasm" \
  --manifest-pin "$COVEN_STAGING_BACKUP_MANIFEST_PIN" \
  --database-module "$modules/staging-recovery.wasm" \
  --database-pin "$COVEN_STAGING_RECOVERY_PIN"
```

Restore creates a new directory. It verifies the manifest and every restored
file before publication. The database snapshot includes committed candidates,
approval and CI receipts, WebAuthn counters, and CI replay records. The backup
also carries the review mirror checkpoint. Backup waits up to 30 seconds
for the checkpoint lock. Between SQLite backup batches it checks a
separate 30-second copy deadline. A timeout leaves the destination
unpublished. Recovery workers stop if the invoking Coven launcher exits.
Restore discards pending approval,
inspection, and enrollment challenges. Witchy validates the manifest and clears
pending state in one SQLite transaction scoped to the two challenge tables. The
backup check has read authority only. TLS and signing keys, the CI nonce key,
and the provisioned trust and identity configuration are managed separately.
[SQLite Online Backup](https://www.sqlite.org/backup.html)

For a migration, stop both private services and local staging writers before
taking the final backup. Start the services against the recovered directory
only after the original writers are stopped. For recovery from an earlier
backup, keep both origins down while reconciling approvals, replay records,
enrollments, and counters against later durable evidence. Replace the CI nonce
key, retire affected runner authorizations, and allow outstanding CI tokens to
expire before accepting fresh submissions. The verifier admits tokens with at
most a 15-minute lifetime. The recovered store reflects the backup's commit
point. Transactions written later require separate recovery or reconciliation.
